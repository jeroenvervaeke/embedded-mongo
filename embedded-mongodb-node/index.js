'use strict';

// The Node.js driver speaks OP_MSG to a socket, so the engine is given one: a listener on a
// Unix socket in a private temporary directory, inside this process, that hands every message
// it reads to the engine and writes the reply back. Nothing of the driver's is replaced or
// reached into, which is what keeps this working across driver versions -- the driver sees a
// standalone server at a socket path, and that is all it needs to see.

const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');

const { Engine } = require('./native');

const SCHEMES = ['mongodb+embedded://', 'mongodb_embedded://'];
const OP_REPLY = 1;
const OP_MSG = 2013;
const HEADER_LENGTH = 16;

/**
 * The database directory an embedded URI names, or `undefined` if this is not an embedded URI.
 * Answering `undefined` rather than throwing is what lets one client class serve both kinds of
 * address: anything this does not recognise is passed to the driver untouched.
 */
function pathFromUri(uri) {
  if (typeof uri !== 'string') return undefined;
  for (const scheme of SCHEMES) {
    if (!uri.startsWith(scheme)) continue;
    const directory = uri.slice(scheme.length);
    if (!directory || directory.includes('?') || directory.includes('#')) {
      throw new Error('embedded MongoDB URI must contain only a database directory');
    }
    return decodeURIComponent(directory);
  }
  return undefined;
}

/** An open directory and the listener serving it. `uri` is what to hand the driver. */
class EmbeddedMongodb {
  #engine;
  #server;
  #sockets = new Set();
  #directory;
  #closed = false;

  constructor(engine) {
    this.#engine = engine;
    // mkdtemp creates the directory 0700, so the socket is reachable by this user alone.
    this.#directory = fs.mkdtempSync(path.join(os.tmpdir(), 'embedded-mongodb-'));
    this.socketPath = path.join(this.#directory, 'mongodb.sock');
    this.uri = `mongodb://${encodeURIComponent(this.socketPath)}/?directConnection=true`;
    this.#server = net.createServer((socket) => {
      this.#sockets.add(socket);
      socket.on('close', () => this.#sockets.delete(socket));
      serve(engine, socket);
    });
    // Binding a Unix socket happens synchronously inside listen(), so the path exists and
    // accepts connections by the time this returns; only the 'listening' event is deferred.
    this.#server.listen(this.socketPath);
  }

  get engine() {
    return this.#engine;
  }

  /** Stops accepting connections, then closes the engine once every command in flight is done. */
  async close() {
    if (this.#closed) return;
    this.#closed = true;
    await new Promise((resolve) => {
      this.#server.close(() => resolve());
      for (const socket of this.#sockets) socket.destroy();
    });
    try {
      await this.#engine.close();
    } finally {
      fs.rmSync(this.#directory, { recursive: true, force: true });
    }
  }

  /** `close` for a caller with no loop to await on, such as a constructor undoing its open. */
  closeSync() {
    if (this.#closed) return;
    this.#closed = true;
    for (const socket of this.#sockets) socket.destroy();
    this.#server.close();
    try {
      this.#engine.closeSync();
    } finally {
      fs.rmSync(this.#directory, { recursive: true, force: true });
    }
  }
}

/** Opens `directory` -- creating it if needed -- and starts serving it. */
async function open(directory) {
  return new EmbeddedMongodb(await Engine.open(directory));
}

/** `open` for a caller with no loop to await on. Blocks for the length of the open. */
function openSync(directory) {
  return new EmbeddedMongodb(Engine.openSync(directory));
}

/**
 * One connection. Messages are answered in the order they arrive, which is the order the
 * driver awaits them in: it never pipelines on one connection, and runs commands in parallel
 * by opening more connections -- each of which lands here separately, and reaches the engine's
 * strand pool alongside the others.
 */
function serve(engine, socket) {
  let pending = Buffer.alloc(0);
  let queue = Promise.resolve();
  let replyId = 0;

  const answer = async (message) => {
    const { requestId, moreToCome, legacy, response } = await engine.roundTrip(message);
    if (moreToCome || socket.destroyed) return;
    const frame = legacy ? replyFrame(response) : msgFrame(response);
    frame.writeInt32LE(frame.length, 0);
    frame.writeInt32LE(++replyId, 4);
    frame.writeInt32LE(requestId, 8);
    socket.write(frame);
  };

  socket.on('data', (chunk) => {
    pending = pending.length === 0 ? chunk : Buffer.concat([pending, chunk]);
    while (pending.length >= 4) {
      const length = pending.readInt32LE(0);
      if (length < HEADER_LENGTH) {
        socket.destroy(new Error(`invalid wire message length ${length}`));
        return;
      }
      if (pending.length < length) break;
      const message = pending.subarray(0, length);
      pending = pending.subarray(length);
      // A failure here is the connection's, as it would be on a real socket: the driver sees
      // it dropped, with the reason, rather than a reply it cannot match to a request.
      queue = queue.then(() => answer(message)).catch((error) => socket.destroy(error));
    }
  });
  // The driver closes connections it is done with, and destroys them on error; neither is
  // anything this side needs to act on beyond the 'close' bookkeeping above.
  socket.on('error', () => {});
}

/** An OP_MSG carrying one body section. The header's length and ids are filled in by the caller. */
function msgFrame(body) {
  const frame = Buffer.allocUnsafe(HEADER_LENGTH + 5 + body.length);
  frame.writeInt32LE(OP_MSG, 12);
  frame.writeUInt32LE(0, 16); // flag bits: a plain reply, nothing more to come
  frame.writeUInt8(0, 20); // section kind: body
  body.copy(frame, HEADER_LENGTH + 5);
  return frame;
}

/** An OP_REPLY carrying one document: what an OP_QUERY handshake is answered with. */
function replyFrame(body) {
  const frame = Buffer.allocUnsafe(HEADER_LENGTH + 20 + body.length);
  frame.writeInt32LE(OP_REPLY, 12);
  frame.writeInt32LE(0, 16); // response flags
  frame.writeBigInt64LE(0n, 20); // cursor id
  frame.writeInt32LE(0, 28); // starting from
  frame.writeInt32LE(1, 32); // number returned
  body.copy(frame, HEADER_LENGTH + 20);
  return frame;
}

/**
 * The driver's `MongoClient`, with embedded URIs routed to the in-process engine. Built on
 * first use rather than at load, so the package loads -- and `open` works -- without the
 * driver installed.
 */
let client;
function mongoClient() {
  if (client) return client;
  let driver;
  try {
    driver = require('mongodb');
  } catch (error) {
    throw new Error('MongoClient needs the mongodb package installed alongside @0q/embedded-mongodb', {
      cause: error,
    });
  }
  client = class MongoClient extends driver.MongoClient {
    #embedded;

    constructor(uri, options) {
      const directory = pathFromUri(uri);
      if (directory === undefined) {
        super(uri, options);
        return;
      }
      // Synchronously, because this is a constructor in the driver's API: the engine has to be
      // serving by the time `connect` can be called, and there is nothing here to await.
      const embedded = openSync(directory);
      try {
        super(embedded.uri, options);
      } catch (error) {
        embedded.closeSync();
        throw error;
      }
      this.#embedded = embedded;
    }

    async close(force) {
      try {
        await super.close(force);
      } finally {
        await this.#embedded?.close();
      }
    }
  };
  return client;
}

module.exports = {
  Engine,
  EmbeddedMongodb,
  open,
  openSync,
  pathFromUri,
  get MongoClient() {
    return mongoClient();
  },
};

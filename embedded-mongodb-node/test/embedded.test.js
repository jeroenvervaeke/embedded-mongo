'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const { describe, it, after } = require('node:test');
const { BSON, MongoClient: DriverMongoClient, MongoServerError } = require('mongodb');

const { Engine, MongoClient, open, openSync, pathFromUri } = require('../index.js');
const { scratch } = require('./support.js');

/** One OP_MSG carrying `command`, as a driver would frame it. */
function opMsg(database, command, requestId = 1) {
  const body = BSON.serialize({ ...command, $db: database });
  const frame = Buffer.alloc(21 + body.length);
  frame.writeInt32LE(frame.length, 0);
  frame.writeInt32LE(requestId, 4);
  frame.writeInt32LE(2013, 12);
  body.copy(frame, 21);
  return frame;
}

describe('pathFromUri', () => {
  it('answers the directory for both schemes', () => {
    assert.equal(pathFromUri('mongodb_embedded://./data'), './data');
    assert.equal(pathFromUri('mongodb+embedded:///var/lib/app'), '/var/lib/app');
  });

  it('decodes percent-encoding', () => {
    assert.equal(pathFromUri('mongodb_embedded://./my%20data'), './my data');
  });

  it('answers undefined for anything that is not an embedded URI', () => {
    assert.equal(pathFromUri('mongodb://localhost'), undefined);
    assert.equal(pathFromUri(undefined), undefined);
    assert.equal(pathFromUri(42), undefined);
  });

  it('rejects a URI carrying more than a directory', () => {
    for (const uri of ['mongodb_embedded://', 'mongodb_embedded://./data?x=1', 'mongodb_embedded://./data#f']) {
      assert.throws(() => pathFromUri(uri), /only a database directory/);
    }
  });
});

describe('open', () => {
  it('serves the driver over the URI it answers', async () => {
    const embedded = await open(scratch());
    const client = new DriverMongoClient(embedded.uri);
    try {
      assert.equal((await client.db('admin').command({ ping: 1 })).ok, 1);
      const items = client.db('app').collection('items');
      const { insertedId } = await items.insertOne({ name: 'embedded' });
      assert.deepEqual(await items.findOne({ _id: insertedId }), { _id: insertedId, name: 'embedded' });
    } finally {
      await client.close();
      await embedded.close();
    }
  });

  it('answers a handshake the driver can run without sessions or streaming', async () => {
    const embedded = await open(scratch());
    const client = new DriverMongoClient(embedded.uri);
    try {
      const hello = await client.db('admin').command({ hello: 1 });
      assert.equal(hello.helloOk, true);
      assert.equal(hello.isWritablePrimary, true);
      assert.equal(hello.logicalSessionTimeoutMinutes, undefined);
      assert.equal(hello.topologyVersion, undefined);
      assert.equal(typeof hello.maxWireVersion, 'number');
    } finally {
      await client.close();
      await embedded.close();
    }
  });

  it('pages a cursor through getMore', async () => {
    const embedded = await open(scratch());
    const client = new DriverMongoClient(embedded.uri);
    try {
      const numbers = client.db('app').collection('numbers');
      await numbers.insertMany(Array.from({ length: 7 }, (_, value) => ({ value })));
      const found = await numbers.find({}, { batchSize: 2, sort: { value: 1 } }).toArray();
      assert.deepEqual(found.map((document) => document.value), [0, 1, 2, 3, 4, 5, 6]);
    } finally {
      await client.close();
      await embedded.close();
    }
  });

  it('runs an aggregation', async () => {
    const embedded = await open(scratch());
    const client = new DriverMongoClient(embedded.uri);
    try {
      const sales = client.db('app').collection('sales');
      await sales.insertMany([{ sku: 'a', qty: 2 }, { sku: 'a', qty: 3 }, { sku: 'b', qty: 1 }]);
      const report = await sales
        .aggregate([{ $group: { _id: '$sku', qty: { $sum: '$qty' } } }, { $sort: { _id: 1 } }])
        .toArray();
      assert.deepEqual(report, [{ _id: 'a', qty: 5 }, { _id: 'b', qty: 1 }]);
    } finally {
      await client.close();
      await embedded.close();
    }
  });

  it('surfaces a command failure as the driver error it would be on a server', async () => {
    const embedded = await open(scratch());
    const client = new DriverMongoClient(embedded.uri);
    try {
      const items = client.db('app').collection('items');
      await items.insertOne({ _id: 1 });
      await assert.rejects(items.insertOne({ _id: 1 }), (error) => error instanceof MongoServerError && error.code === 11000);
    } finally {
      await client.close();
      await embedded.close();
    }
  });

  it('accepts an unacknowledged write, which wants no reply', async () => {
    const embedded = await open(scratch());
    const client = new DriverMongoClient(embedded.uri);
    try {
      const items = client.db('app').collection('items');
      await items.insertOne({ fire: 'forget' }, { writeConcern: { w: 0 } });
      // The engine ran it in order on the same connection, so it is visible to the next command.
      assert.equal(await items.countDocuments(), 1);
    } finally {
      await client.close();
      await embedded.close();
    }
  });

  it('runs commands from many connections at once', async () => {
    const embedded = await open(scratch());
    const client = new DriverMongoClient(embedded.uri, { maxPoolSize: 8 });
    try {
      const database = client.db('app');
      await Promise.all(
        Array.from({ length: 8 }, (_, index) => database.collection(`c${index}`).insertOne({ index }))
      );
      const names = (await database.listCollections().toArray()).map((collection) => collection.name).sort();
      assert.deepEqual(names, ['c0', 'c1', 'c2', 'c3', 'c4', 'c5', 'c6', 'c7']);
    } finally {
      await client.close();
      await embedded.close();
    }
  });

  it('refuses a second engine while one is open', async () => {
    const embedded = await open(scratch());
    try {
      await assert.rejects(open(scratch()), /only one embedded MongoDB runtime may be open per process/);
    } finally {
      await embedded.close();
    }
  });

  it('removes its socket directory on close', async () => {
    const embedded = await open(scratch());
    const socketPath = embedded.socketPath;
    assert.ok(fs.existsSync(socketPath));
    await embedded.close();
    assert.ok(!fs.existsSync(socketPath));
    await embedded.close(); // Idempotent.
  });
});

describe('Engine', () => {
  it('answers a command and refuses one after close', async () => {
    const engine = await Engine.open(scratch());
    const { requestId, moreToCome, legacy, response } = await engine.roundTrip(opMsg('admin', { ping: 1 }, 9));
    assert.equal(requestId, 9);
    assert.equal(moreToCome, false);
    assert.equal(legacy, false);
    assert.equal(BSON.deserialize(response).ok, 1);
    await engine.close();
    await assert.rejects(engine.roundTrip(opMsg('admin', { ping: 1 })), /closed/);
  });

  it('rejects a message that is not a wire message', async () => {
    const engine = await Engine.open(scratch());
    try {
      await assert.rejects(engine.roundTrip(Buffer.from('nope')), /shorter than its header/);
    } finally {
      await engine.close();
    }
  });

  it('opens and closes synchronously', () => {
    const embedded = openSync(scratch());
    assert.match(embedded.uri, /^mongodb:\/\/.*mongodb\.sock\/\?directConnection=true$/);
    embedded.closeSync();
  });
});

describe('MongoClient', () => {
  it('routes an embedded URI to the engine and persists across reopen', async () => {
    const directory = scratch();
    let client = new MongoClient(`mongodb_embedded://${directory}`);
    await client.db('app').collection('items').insertOne({ _id: 'kept' });
    await client.close();

    client = new MongoClient(`mongodb_embedded://${directory}`);
    try {
      assert.deepEqual(await client.db('app').collection('items').findOne(), { _id: 'kept' });
    } finally {
      await client.close();
    }
  });

  it('hands any other URI to the driver untouched', async () => {
    const client = new MongoClient('mongodb://localhost:1/?serverSelectionTimeoutMS=1');
    await client.close();
    // The engine is free, so an embedded client can be opened right after.
    const embedded = new MongoClient(`mongodb_embedded://${scratch()}`);
    await embedded.close();
  });

  it('releases the engine when the driver rejects the options', () => {
    assert.throws(() => new MongoClient(`mongodb_embedded://${scratch()}`, { noSuchOption: 1 }));
    const client = new MongoClient(`mongodb_embedded://${scratch()}`);
    return client.close();
  });
});

# @0q/embedded-mongodb

The MongoDB Node.js driver over an embedded MongoDB engine: the real server code, running
inside your process against a local directory. No server, no port.

```js
const { MongoClient } = require('@0q/embedded-mongodb');

const client = new MongoClient('mongodb_embedded://./data');
const items = client.db('app').collection('items');
await items.insertOne({ name: 'embedded' });
console.log(await items.findOne());
await client.close();
```

`MongoClient` is the driver's own class with one addition: a `mongodb_embedded://<directory>`
(or `mongodb+embedded://`) address opens the directory in-process instead of connecting to a
server. Any other address is handed to the driver untouched. The `mongodb` package is a peer
dependency for this class; the lower layer needs nothing:

```js
const { open } = require('@0q/embedded-mongodb');

const embedded = await open('./data');
// embedded.uri is a mongodb:// address any driver in this process can connect to.
await embedded.close();
```

Only one engine may be open per process. Linux x64, Linux arm64 and macOS arm64 are the
supported platforms. Authentication, TLS, compression, sessions, transactions and change
streams are not supported.

Part of [embedded-mongo](https://github.com/jeroenvervaeke/embedded-mongo), which has the full
documentation, the Python binding and the engine itself. Licensed under the SSPL-1.0, as
MongoDB is.

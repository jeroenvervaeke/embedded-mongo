import type { MongoClient as DriverMongoClient } from 'mongodb';

export { Engine, RoundTrip } from './native';
import type { Engine } from './native';

/**
 * The database directory an embedded URI (`mongodb_embedded://<dir>` or
 * `mongodb+embedded://<dir>`) names, or `undefined` if this is not an embedded URI. Throws if
 * the URI carries anything but a directory.
 */
export declare function pathFromUri(uri: unknown): string | undefined;

/** An open data directory and the in-process listener serving it. */
export declare class EmbeddedMongodb {
  /** A `mongodb://` URI the driver can connect to. */
  readonly uri: string;
  readonly socketPath: string;
  readonly engine: Engine;
  /** Stops accepting connections, then closes the engine once every command in flight is done. */
  close(): Promise<void>;
  /** `close` for a caller with no loop to await on. */
  closeSync(): void;
}

/** Opens `directory`, creating it if needed, and starts serving it. */
export declare function open(directory: string): Promise<EmbeddedMongodb>;

/** `open` for a caller with no loop to await on. Blocks for the length of the open. */
export declare function openSync(directory: string): EmbeddedMongodb;

/**
 * The driver's `MongoClient`, accepting embedded URIs as well as the driver's own. Needs the
 * `mongodb` package installed; everything else here works without it.
 */
export declare const MongoClient: typeof DriverMongoClient;

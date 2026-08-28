"""Turns the NDJSON rows scripts/build-places-seed extracts into a BSON document stream.

    python3 scripts/places-bson-encode.py <rows.ndjson> <places.bson>

Prints the document count. The output is concatenated BSON documents, the shape mongodump
produces, so it can be replayed into a collection without a wrapping container format.

The encoder is a few lines of struct.pack because these documents only ever hold strings,
doubles, subdocuments and one array; pulling in pymongo for that would be a heavier dependency
than the rest of the pipeline put together.
"""

import json
import struct
import sys

ADDRESS_FIELDS = (
    ("street", "addr_street"),
    ("locality", "addr_locality"),
    ("postcode", "addr_postcode"),
    ("region", "addr_region"),
)


def encode_cstring(name):
    return name.encode("utf-8") + b"\x00"


def encode_string(value):
    encoded = value.encode("utf-8")
    return struct.pack("<i", len(encoded) + 1) + encoded + b"\x00"


def encode_element(name, value):
    if isinstance(value, str):
        return b"\x02" + encode_cstring(name) + encode_string(value)
    if isinstance(value, dict):
        return b"\x03" + encode_cstring(name) + encode_document(value.items())
    if isinstance(value, list):
        pairs = ((str(index), item) for index, item in enumerate(value))
        return b"\x04" + encode_cstring(name) + encode_document(pairs)
    if isinstance(value, (int, float)):
        return b"\x01" + encode_cstring(name) + struct.pack("<d", float(value))
    raise TypeError("unsupported BSON value for %s: %r" % (name, value))


def encode_document(pairs):
    body = b"".join(encode_element(name, value) for name, value in pairs)
    return struct.pack("<i", len(body) + 5) + body + b"\x00"


def place(row):
    fields = [("_id", row["id"]), ("name", row["name"]), ("cat", row["cat"])]
    if row.get("brand"):
        fields.append(("brand", row["brand"]))
    fields.append(("confidence", row["confidence"]))
    address = [(key, row[source]) for key, source in ADDRESS_FIELDS if row.get(source)]
    if address:
        fields.append(("addr", dict(address)))
    # GeoJSON puts longitude first, which is the order a 2dsphere index expects.
    fields.append(("loc", {"type": "Point", "coordinates": [row["lon"], row["lat"]]}))
    return encode_document(fields)


def main(source_path, destination_path):
    written = 0
    with open(source_path, encoding="utf-8") as source:
        with open(destination_path, "wb") as destination:
            for line in source:
                destination.write(place(json.loads(line)))
                written += 1
    print(written)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])

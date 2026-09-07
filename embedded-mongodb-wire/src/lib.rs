//! The one OP_MSG parser behind every driver binding.
//!
//! A driver binding hands the engine what a driver would have put on the wire: one OP_MSG,
//! body section and document sequences included, or the one OP_QUERY a driver still opens a
//! connection with. The engine wants a database name and a command document, so this is the
//! translation between the two -- and it lives in a crate of its own because the Python and
//! the Node bindings both need exactly it, and two copies would drift.

use bson::{Bson, Document};

const OP_MSG: i32 = 2013;
const OP_QUERY: i32 = 2004;
const MORE_TO_COME: u32 = 1 << 1;

/// One parsed OP_MSG: what to run, where, and how to answer it.
pub struct Request {
    pub request_id: i32,
    pub more_to_come: bool,
    /// The request was an OP_QUERY, and wants an OP_REPLY rather than an OP_MSG back. Only the
    /// handshake still arrives this way: the Node driver's first `isMaster` on a connection is
    /// one, so that a server too old for OP_MSG can still answer it.
    pub legacy: bool,
    pub database: String,
    /// The body's first key, which is what names a command on the wire.
    pub command_name: String,
    pub command: Vec<u8>,
}

/// Parses one OP_MSG or OP_QUERY. `more_to_come` is the request flag: a driver that set it
/// wants no reply.
pub fn parse(message: &[u8]) -> Result<Request, String> {
    if message.len() < 21 {
        return Err("wire message is shorter than its header and first section".to_owned());
    }
    let declared_len = read_i32(message, 0)?;
    if declared_len < 0 || declared_len as usize != message.len() {
        return Err("wire message length does not match the buffer".to_owned());
    }
    let request_id = read_i32(message, 4)?;
    match read_i32(message, 12)? {
        OP_MSG => parse_msg(message, request_id),
        OP_QUERY => parse_query(message, request_id),
        op_code => Err(format!(
            "embedded MongoDB only accepts OP_MSG and OP_QUERY requests, not op code {op_code}"
        )),
    }
}

fn parse_msg(message: &[u8], request_id: i32) -> Result<Request, String> {
    let flags = read_u32(message, 16)?;
    if flags & !MORE_TO_COME != 0 {
        return Err(format!("unsupported OP_MSG flags: 0x{flags:x}"));
    }

    let mut position = 20;
    let mut body = None;
    let mut sequences = Vec::new();
    while position < message.len() {
        let kind = *message
            .get(position)
            .ok_or_else(|| "missing OP_MSG section kind".to_owned())?;
        position += 1;
        match kind {
            0 => {
                if body.is_some() {
                    return Err("OP_MSG contains more than one body section".to_owned());
                }
                body = Some(read_document(message, &mut position, message.len())?);
            }
            1 => sequences.push(read_sequence(message, &mut position)?),
            _ => return Err(format!("unsupported OP_MSG section kind: {kind}")),
        }
    }

    let mut command = body.ok_or_else(|| "OP_MSG has no body section".to_owned())?;
    for (identifier, documents) in sequences {
        command.insert(identifier, Bson::Array(documents));
    }
    let database = match command.remove("$db") {
        Some(Bson::String(database)) if !database.is_empty() => database,
        Some(_) => return Err("OP_MSG $db must be a non-empty string".to_owned()),
        None => return Err("OP_MSG body has no $db field".to_owned()),
    };
    let command_name = command
        .keys()
        .next()
        .ok_or_else(|| "OP_MSG body names no command".to_owned())?
        .to_owned();
    let command = command
        .to_vec()
        .map_err(|error| format!("failed to encode BSON command: {error}"))?;

    Ok(Request {
        request_id,
        more_to_come: flags & MORE_TO_COME != 0,
        legacy: false,
        database,
        command_name,
        command,
    })
}

/// Header, flags, `<database>.$cmd\0`, numberToSkip, numberToReturn, then the command.
fn parse_query(message: &[u8], request_id: i32) -> Result<Request, String> {
    let mut position = 20;
    let nul = message
        .get(position..)
        .and_then(|rest| rest.iter().position(|byte| *byte == 0))
        .map(|offset| position + offset)
        .ok_or_else(|| "OP_QUERY collection name has no terminator".to_owned())?;
    let collection = std::str::from_utf8(&message[position..nul])
        .map_err(|_| "OP_QUERY collection name is not UTF-8".to_owned())?;
    let Some((database, "$cmd")) = collection.split_once('.') else {
        return Err(format!(
            "OP_QUERY may only carry a command, and `{collection}` is not a `$cmd` collection"
        ));
    };
    if database.is_empty() {
        return Err("OP_QUERY names an empty database".to_owned());
    }
    position = nul + 1 + 8;
    let command = read_document(message, &mut position, message.len())?;
    if position != message.len() {
        return Err("OP_QUERY carries more than one document".to_owned());
    }
    let command_name = command
        .keys()
        .next()
        .ok_or_else(|| "OP_QUERY names no command".to_owned())?
        .to_owned();
    let command = command
        .to_vec()
        .map_err(|error| format!("failed to encode BSON command: {error}"))?;

    Ok(Request {
        request_id,
        more_to_come: false,
        legacy: true,
        database: database.to_owned(),
        command_name,
        command,
    })
}

fn read_sequence(message: &[u8], position: &mut usize) -> Result<(String, Vec<Bson>), String> {
    let section_start = *position;
    let section_len = read_i32(message, section_start)?;
    if section_len < 5 {
        return Err("invalid OP_MSG document-sequence length".to_owned());
    }
    let section_end = section_start
        .checked_add(section_len as usize)
        .filter(|end| *end <= message.len())
        .ok_or_else(|| "OP_MSG document sequence exceeds the buffer".to_owned())?;
    *position += 4;

    let nul = message[*position..section_end]
        .iter()
        .position(|byte| *byte == 0)
        .map(|offset| *position + offset)
        .ok_or_else(|| "OP_MSG document sequence has no identifier terminator".to_owned())?;
    let identifier = std::str::from_utf8(&message[*position..nul])
        .map_err(|_| "OP_MSG document-sequence identifier is not UTF-8".to_owned())?
        .to_owned();
    if identifier.is_empty() {
        return Err("OP_MSG document-sequence identifier is empty".to_owned());
    }
    *position = nul + 1;

    let mut documents = Vec::new();
    while *position < section_end {
        documents.push(Bson::Document(read_document(
            message,
            position,
            section_end,
        )?));
    }
    Ok((identifier, documents))
}

fn read_document(message: &[u8], position: &mut usize, end: usize) -> Result<Document, String> {
    let document_len = read_i32(message, *position)?;
    if document_len < 5 {
        return Err("invalid BSON document length in OP_MSG".to_owned());
    }
    let document_end = position
        .checked_add(document_len as usize)
        .filter(|document_end| *document_end <= end)
        .ok_or_else(|| "BSON document exceeds its OP_MSG section".to_owned())?;
    let document = Document::from_reader(&message[*position..document_end])
        .map_err(|error| format!("invalid BSON document in OP_MSG: {error}"))?;
    *position = document_end;
    Ok(document)
}

fn read_i32(bytes: &[u8], offset: usize) -> Result<i32, String> {
    Ok(i32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(|| "truncated OP_MSG integer".to_owned())?
            .try_into()
            .map_err(|_| "invalid OP_MSG integer".to_owned())?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(|| "truncated OP_MSG integer".to_owned())?
            .try_into()
            .map_err(|_| "invalid OP_MSG integer".to_owned())?,
    ))
}

#[cfg(test)]
mod tests {
    use bson::{Document, doc};

    use super::parse;

    #[test]
    fn parses_body_and_document_sequence() {
        let body = doc! {"insert": "items", "ordered": true, "$db": "app"}
            .to_vec()
            .unwrap();
        let documents = [doc! {"value": 1}, doc! {"value": 2}];
        let identifier = b"documents\0";
        let section_len = 4
            + identifier.len()
            + documents
                .iter()
                .map(|document| document.to_vec().unwrap().len())
                .sum::<usize>();

        let mut message = vec![0; 20];
        message.extend_from_slice(&[0]);
        message.extend_from_slice(&body);
        message.extend_from_slice(&[1]);
        message.extend_from_slice(&(section_len as i32).to_le_bytes());
        message.extend_from_slice(identifier);
        for document in documents {
            message.extend_from_slice(&document.to_vec().unwrap());
        }
        let message_len = message.len() as i32;
        message[0..4].copy_from_slice(&message_len.to_le_bytes());
        message[4..8].copy_from_slice(&42_i32.to_le_bytes());
        message[12..16].copy_from_slice(&2013_i32.to_le_bytes());

        let request = parse(&message).unwrap();
        let command = Document::from_reader(request.command.as_slice()).unwrap();
        assert_eq!(request.request_id, 42);
        assert_eq!(request.database, "app");
        assert_eq!(request.command_name, "insert");
        assert!(!request.legacy);
        assert!(!command.contains_key("$db"));
        assert_eq!(command["documents"].as_array().unwrap().len(), 2);
    }

    /// The shape the Node driver opens every connection with.
    #[test]
    fn parses_a_legacy_handshake() {
        let query = doc! {"isMaster": 1, "helloOk": true}.to_vec().unwrap();
        let mut message = vec![0; 20];
        message.extend_from_slice(b"admin.$cmd\0");
        message.extend_from_slice(&0_i32.to_le_bytes());
        message.extend_from_slice(&(-1_i32).to_le_bytes());
        message.extend_from_slice(&query);
        let message_len = message.len() as i32;
        message[0..4].copy_from_slice(&message_len.to_le_bytes());
        message[4..8].copy_from_slice(&7_i32.to_le_bytes());
        message[12..16].copy_from_slice(&2004_i32.to_le_bytes());

        let request = parse(&message).unwrap();
        let command = Document::from_reader(request.command.as_slice()).unwrap();
        assert_eq!(request.request_id, 7);
        assert!(request.legacy);
        assert_eq!(request.database, "admin");
        assert_eq!(request.command_name, "isMaster");
        assert_eq!(command, doc! {"isMaster": 1, "helloOk": true});
    }

    #[test]
    fn rejects_a_legacy_query_that_is_not_a_command() {
        let query = doc! {"a": 1}.to_vec().unwrap();
        let mut message = vec![0; 20];
        message.extend_from_slice(b"app.items\0");
        message.extend_from_slice(&[0; 8]);
        message.extend_from_slice(&query);
        let message_len = message.len() as i32;
        message[0..4].copy_from_slice(&message_len.to_le_bytes());
        message[12..16].copy_from_slice(&2004_i32.to_le_bytes());
        assert!(parse(&message).err().unwrap().contains("$cmd"));
    }

    #[test]
    fn rejects_other_op_codes() {
        let mut message = vec![0; 21];
        message[0..4].copy_from_slice(&21_i32.to_le_bytes());
        message[12..16].copy_from_slice(&2012_i32.to_le_bytes());
        assert!(parse(&message).err().unwrap().contains("2012"));
    }

    #[test]
    fn rejects_unsupported_flags() {
        let body = doc! {"ping": 1, "$db": "admin"}.to_vec().unwrap();
        let mut message = vec![0; 21];
        message.extend_from_slice(&body);
        let message_len = message.len() as i32;
        message[0..4].copy_from_slice(&message_len.to_le_bytes());
        message[12..16].copy_from_slice(&2013_i32.to_le_bytes());
        message[16..20].copy_from_slice(&1_u32.to_le_bytes());
        assert!(parse(&message).err().unwrap().contains("flags"));
    }
}

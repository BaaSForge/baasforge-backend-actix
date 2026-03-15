use uuid::Uuid;

#[inline]
pub fn new_uuid() -> Uuid {
    Uuid::parse_str(&uuidv7::create()).expect("uuidv7::create returns valid UUID string")
}

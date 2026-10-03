use thiserror::Error;

#[derive(Debug, Error)]
pub enum WdbError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid magic string: expected {expected:?}, got {got:?}")]
    InvalidMagic { expected: String, got: String },

    #[error("Unsupported type table kind: 0x{0:02X}")]
    UnsupportedTypeKind(u8),

    #[error("Invalid scope definition kind: 0x{0:02X}")]
    InvalidScopeKind(u32),

    #[error("Decompression error: {0}")]
    Decompression(String),

    #[error("Type index {0} out of bounds (have {1} types)")]
    TypeIndexOutOfBounds(u32, usize),

    #[error("Scope index {0} out of bounds")]
    ScopeIndexOutOfBounds(usize),

    #[error("Variable index {0} out of bounds")]
    VarIndexOutOfBounds(usize),

    #[error("Parse error: {0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, WdbError>;

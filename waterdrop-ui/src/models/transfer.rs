/// Represents the status of a file or message transfer.
#[derive(Debug, Clone, PartialEq)]
pub enum TransferStatus {
    Idle,
    Sending { progress: f64, filename: String },
    Done { filename: String },
    Error { message: String },
}

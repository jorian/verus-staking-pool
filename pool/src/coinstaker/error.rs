use std::fmt::Formatter;

#[derive(Debug)]
pub enum CoinStakerError {
    SubscriberAlreadyExists,
}

impl std::error::Error for CoinStakerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }

    fn cause(&self) -> Option<&dyn std::error::Error> {
        self.source()
    }
}

impl std::fmt::Display for CoinStakerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::SubscriberAlreadyExists => write!(f, "Subscriber already exists"),
        }
    }
}

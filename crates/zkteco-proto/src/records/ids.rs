//! The two user identities a device keeps, as distinct types so they cannot
//! be mixed up.

use std::fmt;

/// The device-internal user index (`uid`): a slot number assigned by the
/// device, used by commands that address a stored user or template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uid(pub u16);

impl fmt::Display for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The user ID shown on the device and entered by people (the "PIN" or
/// employee number). Text on most layouts; a number on 28-byte records.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct UserId(pub String);

impl UserId {
    /// The ID as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for UserId {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for UserId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for UserId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for UserId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for UserId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

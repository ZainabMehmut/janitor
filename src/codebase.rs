//! Codebase names.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A name that fits the `codebase_name` domain in `schema/state.sql`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CodebaseName(String);

/// A string that is not a valid [`CodebaseName`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidCodebaseName(String);

impl fmt::Display for InvalidCodebaseName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid codebase name: {:?}", self.0)
    }
}

impl std::error::Error for InvalidCodebaseName {}

impl CodebaseName {
    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CodebaseName {
    type Error = InvalidCodebaseName;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        // `similar to '[a-z0-9][a-z0-9+-.]+'`; `+-.` is a range, so it takes `,` too.
        let mut chars = name.chars();
        let first = chars
            .next()
            .is_some_and(|c| matches!(c, 'a'..='z' | '0'..='9'));
        if first && name.len() > 1 && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '+'..='.'))
        {
            Ok(Self(name))
        } else {
            Err(InvalidCodebaseName(name))
        }
    }
}

impl TryFrom<&str> for CodebaseName {
    type Error = InvalidCodebaseName;

    fn try_from(name: &str) -> Result<Self, Self::Error> {
        Self::try_from(name.to_string())
    }
}

impl FromStr for CodebaseName {
    type Err = InvalidCodebaseName;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::try_from(name)
    }
}

impl From<CodebaseName> for String {
    fn from(name: CodebaseName) -> Self {
        name.0
    }
}

impl AsRef<str> for CodebaseName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<Path> for CodebaseName {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl fmt::Display for CodebaseName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &[&str] = &["codebase", "0ad", "a.b", "a,b", "a+b", "a-b", "x.."];
    const INVALID: &[&str] = &[
        "", "a", ".", "..", "../a", "a/b", "/etc", ".hidden", "-a", "Foo", "a_b", "a b", "a\0b",
    ];

    #[test]
    fn test_valid_names() {
        for name in VALID {
            let parsed: CodebaseName = name.parse().unwrap();
            assert_eq!(parsed.as_str(), *name);
        }
    }

    #[test]
    fn test_invalid_names() {
        for name in INVALID {
            assert_eq!(
                CodebaseName::try_from(*name),
                Err(InvalidCodebaseName(name.to_string())),
            );
        }
    }

    #[test]
    fn test_serde() {
        let name: CodebaseName = serde_json::from_str("\"codebase\"").unwrap();
        assert_eq!(name.as_str(), "codebase");
        assert_eq!(serde_json::to_string(&name).unwrap(), "\"codebase\"");
        let err = serde_json::from_str::<CodebaseName>("\"../etc\"").unwrap_err();
        assert_eq!(err.to_string(), "invalid codebase name: \"../etc\"");
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn test_matches_schema_domain() {
        let Some(db) = crate::test_utils::TestDatabase::new_optional()
            .await
            .unwrap()
        else {
            return;
        };
        crate::schema::setup_test_database(db.pool()).await.unwrap();
        let mut accepted = Vec::new();
        for name in VALID.iter().chain(INVALID) {
            let in_domain = sqlx::query("SELECT $1::codebase_name")
                .bind(*name)
                .execute(db.pool())
                .await
                .is_ok();
            accepted.push((*name, in_domain));
        }
        let parsed: Vec<_> = VALID
            .iter()
            .chain(INVALID)
            .map(|name| (*name, CodebaseName::try_from(*name).is_ok()))
            .collect();
        assert_eq!(accepted, parsed);
    }
}

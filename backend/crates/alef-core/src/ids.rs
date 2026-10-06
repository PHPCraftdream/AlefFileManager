// SPDX-License-Identifier: MIT OR Apache-2.0
//! Opaque identifiers shared by sessions, resources, streams and the registry.
use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        // ts-rs does not know `serde(transparent)` and prints a false-positive warning:
        // the derived newtype alias to `number` already matches transparent serde exactly.
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
            ts_rs::TS,
        )]
        #[serde(transparent)]
        #[ts(export, export_to = "core.ts")]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(
    /// A document session: one window plus one page load.
    SessionId
);
id_type!(
    /// A resource (file, socket, database, process...) owned by a session.
    ResourceId
);
id_type!(
    /// A transport stream between the page and the runtime.
    StreamId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_serialize_as_plain_numbers_and_display() {
        assert_eq!(serde_json::to_string(&StreamId(7)).expect("serialize"), "7");
        assert_eq!(SessionId(3).to_string(), "3");
        assert_eq!(
            serde_json::from_str::<ResourceId>("9").expect("deserialize"),
            ResourceId(9)
        );
    }

    fn generated_ts(file: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../packages/api/types")
                .join(file),
        )
        .expect("generated types must exist; run `npm run gen:types`")
    }

    #[test]
    fn generated_ts_ids_are_json_numbers() {
        let core = generated_ts("core.ts");
        for name in ["SessionId", "ResourceId", "StreamId"] {
            assert!(
                core.contains(&format!("export type {name} = number;")),
                "{name} must be a JSON number in TS (ids travel as JSON numbers, 2^53 precision \
                 limit), not bigint"
            );
        }
    }
}

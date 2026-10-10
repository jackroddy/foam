//! The steps that bring a ref's files from the schema they were
//! written in up to the one this foam reads.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde_json::Value;

use crate::model::SCHEMA_VERSION;

/// A ref's files as JSON, by path.
pub type Files = BTreeMap<String, Value>;

/// How many schemas back this foam migrates from; a step older
/// than that is dropped and its schema retired.
pub const WINDOW: u32 = 3;

/// The foam that first wrote each schema.
const INTRODUCED: &[(u32, &str)] = &[(1, "0.1.0")];

/// The first foam that writes `schema`.
pub fn introduced(schema: u32) -> &'static str {
    INTRODUCED
        .iter()
        .find(|(s, _)| *s == schema)
        .map_or("a later foam", |(_, v)| v)
}

/// One step, from schema `from` to the next.
struct Step {
    from: u32,
    run: fn(&mut Files) -> Result<()>,
}

/// The steps this foam carries, oldest first.
const STEPS: &[Step] = &[];

/// Schemas this foam no longer migrates from, each with the last
/// foam that did.
const RETIRED: &[(u32, &str)] = &[];

/// Bring `files`, written at schema `from`, up to this foam's.
pub fn upgrade(files: &mut Files, from: u32) -> Result<()> {
    chain(STEPS, RETIRED, files, from, SCHEMA_VERSION, WINDOW)
}

fn chain(
    steps: &[Step],
    retired: &[(u32, &str)],
    files: &mut Files,
    from: u32,
    to: u32,
    window: u32,
) -> Result<()> {
    if from > to {
        bail!("data is schema {from} but this foam reads up to {to}");
    }
    if from + window < to {
        let through = retired
            .iter()
            .find(|(s, _)| *s == from)
            .map_or("an earlier foam".to_string(), |(_, last)| {
                format!("foam {last}")
            });
        bail!(
            "data is schema {from}, older than the {window} schemas this foam migrates from; install {through}, run any command there, then come back"
        );
    }
    for at in from..to {
        let Some(step) = steps.iter().find(|s| s.from == at) else {
            bail!("data is schema {at} and this foam has no step from it");
        };
        (step.run)(files)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(files: &mut Files, key: &str) {
        files
            .entry("meta.json".into())
            .or_insert_with(|| Value::Object(Default::default()))[key] = Value::Bool(true);
    }

    const FAKE: &[Step] = &[
        Step {
            from: 1,
            run: |f| {
                mark(f, "one");
                Ok(())
            },
        },
        Step {
            from: 2,
            run: |f| {
                mark(f, "two");
                Ok(())
            },
        },
    ];

    #[test]
    fn the_chain_runs_every_step_from_the_ref_up_to_the_binary() {
        let mut files = Files::new();
        chain(FAKE, &[], &mut files, 1, 3, 3).unwrap();
        assert_eq!(files["meta.json"]["one"], true);
        assert_eq!(files["meta.json"]["two"], true);
        let mut files = Files::new();
        chain(FAKE, &[], &mut files, 2, 3, 3).unwrap();
        assert!(files["meta.json"].get("one").is_none());
        assert_eq!(files["meta.json"]["two"], true);
        // at the binary's schema nothing runs
        let mut files = Files::new();
        chain(FAKE, &[], &mut files, 3, 3, 3).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn a_newer_or_retired_schema_is_refused_with_the_way_through() {
        let mut files = Files::new();
        let err = chain(FAKE, &[], &mut files, 4, 3, 3)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "data is schema 4 but this foam reads up to 3");
        // outside the window the retired table names the way through
        let err = chain(FAKE, &[(1, "0.4.0")], &mut files, 1, 3, 1)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("older than the 1 schemas") && err.contains("install foam 0.4.0"),
            "{err}"
        );
        let err = chain(FAKE, &[], &mut files, 1, 3, 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("install an earlier foam"), "{err}");
        // inside the window a missing step is a bug, said plainly
        let err = chain(&FAKE[1..], &[], &mut files, 1, 3, 3)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no step from it"), "{err}");
    }

    #[test]
    fn every_step_is_within_the_window_and_every_schema_has_a_foam() {
        for step in STEPS {
            assert!(
                step.from + WINDOW >= SCHEMA_VERSION,
                "step from {}",
                step.from
            );
        }
        for s in 1..=SCHEMA_VERSION {
            assert_ne!(introduced(s), "a later foam", "schema {s}");
        }
    }

    #[test]
    fn this_foam_reads_its_own_schema_without_a_step() {
        let mut files = Files::new();
        upgrade(&mut files, SCHEMA_VERSION).unwrap();
        assert!(upgrade(&mut files, SCHEMA_VERSION + 1).is_err());
    }
}

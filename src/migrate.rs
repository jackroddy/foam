//! The steps that bring a ref's files from the schema they were
//! written in up to the one this foam reads.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde_json::Value;

use crate::model::{SCHEMA_VERSION, note_id_of};

/// A ref's files as JSON, by path.
pub type Files = BTreeMap<String, Value>;

/// How many schemas back this foam migrates from; a step older
/// than that is dropped and its schema retired.
pub const WINDOW: u32 = 3;

/// The foam that first wrote each schema.
const INTRODUCED: &[(u32, &str)] = &[(1, "0.1.0"), (2, "0.4.0"), (3, "0.5.0")];

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
const STEPS: &[Step] = &[
    Step {
        from: 1,
        run: notes_get_ids,
    },
    Step {
        from: 2,
        run: notes_may_answer,
    },
];

/// Schema 3: a note may answer another. Nothing to rewrite; the
/// number alone keeps a foam that drops the field from writing.
fn notes_may_answer(_: &mut Files) -> Result<()> {
    Ok(())
}

/// Schema 2: every note has an id.
fn notes_get_ids(files: &mut Files) -> Result<()> {
    for (path, value) in files.iter_mut() {
        if !path.starts_with("issues/") {
            continue;
        }
        let Some(notes) = value.get_mut("notes").and_then(Value::as_array_mut) else {
            continue;
        };
        for note in notes {
            if note.get("id").is_some() {
                continue;
            }
            let field = |k: &str| {
                note.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let id = note_id_of(
                &field("at"),
                &field("author"),
                &field("text"),
                &field("commit"),
                &field("branch"),
            );
            note["id"] = Value::String(id);
        }
    }
    Ok(())
}

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
    fn schema_two_gives_every_old_note_the_same_id_everywhere() {
        let mut files = Files::new();
        files.insert(
            "issues/t-1.json".into(),
            serde_json::json!({
                "id": "t-1",
                "notes": [
                    {"at": "2026-09-26T10:00:00Z", "author": "ann", "text": "one",
                     "commit": "abc", "branch": "main"},
                    {"id": "deadbeef", "at": "2026-09-26T11:00:00Z", "author": "ann",
                     "text": "two", "commit": "abc", "branch": "main"}
                ]
            }),
        );
        files.insert("memories/m.json".into(), serde_json::json!({"slug": "m"}));
        upgrade(&mut files, 1).unwrap();
        let notes = files["issues/t-1.json"]["notes"].as_array().unwrap();
        // the hash is pinned: a change here would give every clone a
        // different id for the same old note
        assert_eq!(notes[0]["id"], "e55bb7a7");
        assert_eq!(notes[1]["id"], "deadbeef");
        assert_eq!(
            note_id_of("2026-09-26T10:00:00Z", "ann", "one", "abc", "main"),
            "e55bb7a7"
        );
        assert_ne!(
            note_id_of("2026-09-26T10:00:00Z", "ann", "one", "abc", "dev"),
            "e55bb7a7"
        );
    }

    #[test]
    fn this_foam_reads_its_own_schema_without_a_step() {
        let mut files = Files::new();
        upgrade(&mut files, SCHEMA_VERSION).unwrap();
        assert!(upgrade(&mut files, SCHEMA_VERSION + 1).is_err());
    }
}

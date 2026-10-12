// SPDX-License-Identifier: MIT OR Apache-2.0
//! The end-to-end hooks of consent: the table of answers and the clicks of a user come from the
//! environment, and only with `ALEF_E2E=1`. Compiled only with the cargo feature `e2e`.
use super::{ask, Asker, Script, SCRIPT_VARIABLE, UI_VARIABLE};

fn environment(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub(super) fn scripted_asker() -> Result<Option<Asker>, String> {
    scripted_asker_from(&environment)
}

fn scripted_asker_from(read: &dyn Fn(&str) -> Option<String>) -> Result<Option<Asker>, String> {
    if read("ALEF_E2E").is_some_and(|value| value == "1") {
        if let Some(text) = read(SCRIPT_VARIABLE) {
            return Script::parse(&text)
                .map(|script| Some(Asker::Script(script)))
                .map_err(|error| format!("{SCRIPT_VARIABLE}: {error}"));
        }
    }
    Ok(None)
}

pub(super) fn window_asker() -> Result<Asker, String> {
    window_asker_from(&environment)
}

fn window_asker_from(read: &dyn Fn(&str) -> Option<String>) -> Result<Asker, String> {
    let automation = if read("ALEF_E2E").is_some_and(|value| value == "1") {
        read(UI_VARIABLE)
            .map(|text| {
                serde_json::from_str(&text).map_err(|error| format!("{UI_VARIABLE}: {error}"))
            })
            .transpose()?
    } else {
        None
    };
    Ok(Asker::Window { automation })
}

pub(super) fn with_automation(mut request: ask::Request, asker: &Asker) -> ask::Request {
    if let Asker::Window { automation } = asker {
        request.automation = automation.clone();
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(table: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            table
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn the_answers_of_a_script_are_read_only_in_an_end_to_end_run() {
        let answers = "clipboard.read=deny";
        let silent = pairs(&[("ALEF_E2E_CONSENT", "clipboard.read=deny")]);
        assert_eq!(scripted_asker_from(&silent).unwrap(), None);
        let off = pairs(&[
            ("ALEF_E2E", "0"),
            ("ALEF_E2E_CONSENT", "clipboard.read=deny"),
        ]);
        assert_eq!(scripted_asker_from(&off).unwrap(), None);
        let on = pairs(&[
            ("ALEF_E2E", "1"),
            ("ALEF_E2E_CONSENT", "clipboard.read=deny"),
        ]);
        assert_eq!(
            scripted_asker_from(&on).unwrap(),
            Some(Asker::Script(Script::parse(answers).unwrap()))
        );
        let nothing = pairs(&[("ALEF_E2E", "1")]);
        assert_eq!(scripted_asker_from(&nothing).unwrap(), None);
    }

    #[test]
    fn the_clicks_of_a_user_are_read_only_in_an_end_to_end_run() {
        let clicks = r#"[{"submit":true}]"#;
        let off = pairs(&[("ALEF_E2E_CONSENT_UI", r#"[{"submit":true}]"#)]);
        assert_eq!(
            window_asker_from(&off).unwrap(),
            Asker::Window { automation: None }
        );
        let on = pairs(&[
            ("ALEF_E2E", "1"),
            ("ALEF_E2E_CONSENT_UI", r#"[{"submit":true}]"#),
        ]);
        assert_eq!(
            window_asker_from(&on).unwrap(),
            Asker::Window {
                automation: Some(serde_json::from_str(clicks).unwrap())
            }
        );
    }
}

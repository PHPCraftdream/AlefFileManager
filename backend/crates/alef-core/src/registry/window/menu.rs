// SPDX-License-Identifier: MIT OR Apache-2.0
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::ids::SessionId;

pub const NODE_LIMIT: usize = 256;
pub const DEPTH_LIMIT: usize = 8;
pub const ID_LIMIT: usize = 128;
pub const LABEL_LIMIT: usize = 256;
pub const ACCELERATOR_LIMIT: usize = 128;
pub const COORDINATE_LIMIT: f64 = 1.0e6;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "core.ts")]
pub enum MenuKind {
    #[default]
    Normal,
    Check,
    Separator,
    Submenu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "core.ts")]
pub enum MenuRole {
    Copy,
    Paste,
    Cut,
    Undo,
    Redo,
    SelectAll,
    Quit,
    About,
    Minimize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "core.ts")]
pub struct MenuItem {
    #[ts(optional)]
    pub kind: Option<MenuKind>,
    #[ts(optional)]
    pub id: Option<String>,
    #[ts(optional)]
    pub label: Option<String>,
    #[ts(optional)]
    pub role: Option<MenuRole>,
    #[ts(optional)]
    pub enabled: Option<bool>,
    #[ts(optional)]
    pub checked: Option<bool>,
    #[ts(optional)]
    pub accelerator: Option<String>,
    #[ts(optional)]
    pub items: Option<Vec<MenuItem>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MenuCall {
    SetApplication {
        owner: SessionId,
        items: Vec<MenuItem>,
    },
    SetWindow {
        owner: SessionId,
        label: Option<String>,
        items: Vec<MenuItem>,
    },
    Popup {
        owner: SessionId,
        label: Option<String>,
        items: Vec<MenuItem>,
        x: Option<f64>,
        y: Option<f64>,
    },
    Release {
        owner: SessionId,
    },
}

fn text(name: &str, value: &str, limit: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(format!("{name} must be 1..{limit} bytes without controls"));
    }
    Ok(())
}

impl MenuItem {
    pub fn effective_kind(&self) -> MenuKind {
        self.kind.unwrap_or_default()
    }

    fn check<'a>(&'a self, ids: &mut HashSet<&'a str>) -> Result<(), String> {
        let kind = self.effective_kind();
        if let Some(label) = &self.label {
            text("label", label, LABEL_LIMIT)?;
        } else if kind != MenuKind::Separator && self.role.is_none() {
            return Err("a custom menu item needs a label".to_owned());
        }
        if let Some(id) = &self.id {
            text("id", id, ID_LIMIT)?;
            if !ids.insert(id) {
                return Err("menu ids must be unique across the whole tree".to_owned());
            }
        } else if kind != MenuKind::Separator && self.role.is_none() {
            return Err("a custom menu item needs an id".to_owned());
        }
        if self.role.is_some()
            && (kind != MenuKind::Normal
                || self.id.is_some()
                || self.checked.is_some()
                || self.items.is_some()
                || self.accelerator.is_some())
        {
            return Err(
                "a role needs normal kind and no id, checked, items or accelerator".to_owned(),
            );
        }
        if self.checked.is_some() && kind != MenuKind::Check {
            return Err("checked is only valid for check items".to_owned());
        }
        if self.items.is_some() && kind != MenuKind::Submenu {
            return Err("items is only valid for submenus".to_owned());
        }
        if kind == MenuKind::Submenu && self.items.is_none() {
            return Err("a submenu needs items".to_owned());
        }
        if kind == MenuKind::Separator
            && (self.id.is_some()
                || self.role.is_some()
                || self.enabled.is_some()
                || self.checked.is_some()
                || self.items.is_some()
                || self.accelerator.is_some())
        {
            return Err("a separator cannot have actionable fields".to_owned());
        }
        if let Some(accelerator) = &self.accelerator {
            if !matches!(kind, MenuKind::Normal | MenuKind::Check) {
                return Err("accelerator is only valid for normal or check items".to_owned());
            }
            text("accelerator", accelerator, ACCELERATOR_LIMIT)?;
        }
        Ok(())
    }
}

pub fn check_items(items: &[MenuItem]) -> Result<(), String> {
    fn visit<'a>(
        items: &'a [MenuItem],
        depth: usize,
        count: &mut usize,
        ids: &mut HashSet<&'a str>,
    ) -> Result<(), String> {
        if !items.is_empty() && depth > DEPTH_LIMIT {
            return Err(format!("menu exceeds {DEPTH_LIMIT} levels"));
        }
        for item in items {
            if *count == NODE_LIMIT {
                return Err(format!("menu exceeds {NODE_LIMIT} total nodes"));
            }
            *count += 1;
            item.check(ids)?;
            if let Some(children) = &item.items {
                visit(children, depth + 1, count, ids)?;
            }
        }
        Ok(())
    }
    visit(items, 1, &mut 0, &mut HashSet::new())
}

impl MenuCall {
    pub fn check(&self) -> Result<(), String> {
        match self {
            Self::SetApplication { items, .. } | Self::SetWindow { items, .. } => {
                check_items(items)
            }
            Self::Popup { items, x, y, .. } => {
                check_items(items)?;
                if x.is_some() != y.is_some() {
                    return Err("x and y come together or not at all".to_owned());
                }
                for (name, coordinate) in [("x", x), ("y", y)] {
                    if coordinate.is_some_and(|v| !v.is_finite() || v.abs() > COORDINATE_LIMIT) {
                        return Err(format!(
                            "{name} must be finite and within +/-{COORDINATE_LIMIT}"
                        ));
                    }
                }
                Ok(())
            }
            Self::Release { .. } => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use ts_rs::TS;

    fn item(id: &str) -> MenuItem {
        MenuItem {
            id: Some(id.to_owned()),
            label: Some("Action".to_owned()),
            ..MenuItem::default()
        }
    }

    fn parse(value: Value) -> MenuItem {
        serde_json::from_value(value).expect("menu DTO")
    }

    fn valid(value: Value) {
        assert!(check_items(&[parse(value)]).is_ok());
    }

    fn invalid(value: Value) {
        assert!(check_items(&[parse(value)]).is_err());
    }

    fn nested(levels: usize) -> Vec<MenuItem> {
        let mut items = vec![item("leaf")];
        for level in 1..levels {
            items = vec![MenuItem {
                kind: Some(MenuKind::Submenu),
                items: Some(items),
                ..item(&format!("level-{level}"))
            }];
        }
        items
    }

    #[test]
    fn dto_round_trip_and_defaults() {
        let value = json!({"kind":"submenu","id":"file","label":"File","enabled":true,
            "items":[{"kind":"check","id":"visible","label":"Visible","checked":false,
                "accelerator":"Ctrl+V"},{"role":"selectAll"},{"kind":"separator"}]});
        let menu = parse(value.clone());
        assert!(check_items(std::slice::from_ref(&menu)).is_ok());
        assert_eq!(parse(serde_json::to_value(&menu).expect("serialize")), menu);
        assert_eq!(item("default").effective_kind(), MenuKind::Normal);
        assert_eq!(
            parse(json!({"role":"copy"})).effective_kind(),
            MenuKind::Normal
        );
    }

    #[test]
    fn unknown_fields_are_rejected_recursively() {
        for value in [
            json!({"id":"a","label":"A","typo":true}),
            json!({"kind":"submenu","id":"a","label":"A",
                "items":[{"role":"copy","typo":true}]}),
        ] {
            assert!(serde_json::from_value::<MenuItem>(value).is_err());
        }
        for value in [json!({"kind":"unknown"}), json!({"role":"unknown"})] {
            assert!(serde_json::from_value::<MenuItem>(value).is_err());
        }
    }

    #[test]
    fn generated_type_shape_matches_optional_recursive_dto() {
        let declaration = MenuItem::decl(&ts_rs::Config::default());
        for field in [
            "kind?",
            "id?",
            "label?",
            "role?",
            "enabled?",
            "checked?",
            "accelerator?",
            "items?",
        ] {
            assert!(declaration.contains(field), "{declaration}");
        }
        assert!(declaration.contains("Array<MenuItem>"), "{declaration}");
        assert!(MenuRole::decl(&ts_rs::Config::default()).contains("\"selectAll\""));
        assert!(MenuKind::decl(&ts_rs::Config::default()).contains("\"submenu\""));
    }

    #[test]
    fn total_node_limit_counts_all_kinds_and_descendants() {
        let separator = parse(json!({"kind":"separator"}));
        assert!(check_items(&vec![separator.clone(); 256]).is_ok());
        assert!(check_items(&vec![separator.clone(); 257]).is_err());
        let mut root = item("root");
        root.kind = Some(MenuKind::Submenu);
        root.items = Some(vec![separator.clone(); 255]);
        assert!(check_items(std::slice::from_ref(&root)).is_ok());
        root.items.as_mut().expect("children").push(separator);
        assert!(check_items(&[root]).is_err());
    }

    #[test]
    fn depth_limit_counts_root_as_level_one() {
        assert!(check_items(&nested(8)).is_ok());
        assert!(check_items(&nested(9)).is_err());
        let mut tree = nested(8);
        let mut leaf = &mut tree[0];
        while leaf.items.is_some() {
            leaf = &mut leaf.items.as_mut().expect("children")[0];
        }
        leaf.kind = Some(MenuKind::Submenu);
        leaf.items = Some(vec![]);
        assert!(check_items(&tree).is_ok());
    }

    #[test]
    fn custom_items_require_id_and_label() {
        for kind in ["normal", "check", "submenu"] {
            let mut value = json!({"kind":kind,"id":"a","label":"A"});
            if kind == "submenu" {
                value["items"] = json!([]);
            }
            valid(value.clone());
            let mut no_id = value.clone();
            no_id.as_object_mut().expect("object").remove("id");
            invalid(no_id);
            value.as_object_mut().expect("object").remove("label");
            invalid(value);
        }
    }

    #[test]
    fn id_limits_are_bytes_and_reject_unicode_controls() {
        for id in ["a".to_owned(), "a".repeat(128), "é".repeat(64)] {
            assert!(check_items(&[item(&id)]).is_ok());
        }
        for id in [
            "".to_owned(),
            "a".repeat(129),
            "é".repeat(65),
            "a\n".to_owned(),
            "a\u{85}".to_owned(),
        ] {
            assert!(check_items(&[item(&id)]).is_err());
        }
    }

    #[test]
    fn ids_are_unique_across_siblings_submenus_and_kinds() {
        assert!(check_items(&[item("a"), item("a")]).is_err());
        invalid(json!({"kind":"submenu","id":"a","label":"A",
            "items":[{"id":"a","label":"Child"}]}));
        let branch = |id: &str| {
            parse(json!({"kind":"submenu","id":id,"label":"Branch",
            "items":[{"kind":"check","id":"shared","label":"Child"}]}))
        };
        assert!(check_items(&[branch("left"), branch("right")]).is_err());
        assert!(check_items(&[item("a"), item("A")]).is_ok());
    }

    #[test]
    fn label_limits_apply_to_optional_separator_and_role_labels() {
        for base in [
            json!({"id":"a"}),
            json!({"role":"copy"}),
            json!({"kind":"separator"}),
        ] {
            for label in ["a".repeat(256), "é".repeat(128)] {
                let mut value = base.clone();
                value["label"] = json!(label);
                valid(value);
            }
            for label in [
                "".to_owned(),
                "a".repeat(257),
                "é".repeat(129),
                "\t".to_owned(),
                "\u{7f}".to_owned(),
            ] {
                let mut value = base.clone();
                value["label"] = json!(label);
                invalid(value);
            }
        }
    }

    #[test]
    fn roles_allow_omitted_labels_and_no_custom_action_fields() {
        for role in [
            "copy",
            "paste",
            "cut",
            "undo",
            "redo",
            "selectAll",
            "quit",
            "about",
            "minimize",
        ] {
            valid(json!({"role":role}));
            valid(json!({"role":role,"label":"Native","enabled":false}));
            for (field, value) in [
                ("id", json!("a")),
                ("checked", json!(false)),
                ("items", json!([])),
                ("accelerator", json!("Ctrl+C")),
                ("kind", json!("check")),
                ("kind", json!("separator")),
                ("kind", json!("submenu")),
            ] {
                let mut item = json!({"role":role});
                item[field] = value;
                invalid(item);
            }
        }
    }

    #[test]
    fn kind_consistency_rejects_even_false_or_empty_fields() {
        invalid(json!({"id":"a","label":"A","checked":false}));
        invalid(json!({"id":"a","label":"A","items":[]}));
        invalid(json!({"kind":"check","id":"a","label":"A","items":[]}));
        invalid(json!({"kind":"submenu","id":"a","label":"A"}));
        invalid(json!({"kind":"submenu","id":"a","label":"A","items":[],"checked":false}));
        invalid(json!({"kind":"submenu","id":"a","label":"A","items":[],"accelerator":"Ctrl+A"}));
        valid(json!({"kind":"check","id":"a","label":"A"}));
        valid(json!({"kind":"check","id":"a","label":"A","checked":false}));
        valid(json!({"kind":"check","id":"a","label":"A","checked":true}));
    }

    #[test]
    fn separators_have_no_actionable_fields() {
        valid(json!({"kind":"separator"}));
        for (field, value) in [
            ("id", json!("a")),
            ("role", json!("copy")),
            ("enabled", json!(false)),
            ("checked", json!(false)),
            ("items", json!([])),
            ("accelerator", json!("Ctrl+C")),
        ] {
            let mut item = json!({"kind":"separator"});
            item[field] = value;
            invalid(item);
        }
    }

    #[test]
    fn accelerators_have_lexical_not_native_syntax_validation() {
        for accelerator in [
            "Ctrl+C".to_owned(),
            "not native syntax".to_owned(),
            "a".repeat(128),
            "é".repeat(64),
        ] {
            valid(json!({"id":"a","label":"A","accelerator":accelerator}));
        }
        for accelerator in [
            "".to_owned(),
            "a".repeat(129),
            "é".repeat(65),
            "Ctrl+\n".to_owned(),
            "\u{85}".to_owned(),
        ] {
            invalid(json!({"id":"a","label":"A","accelerator":accelerator}));
        }
    }

    fn popup(x: Option<f64>, y: Option<f64>) -> MenuCall {
        MenuCall::Popup {
            owner: SessionId(7),
            label: None,
            items: vec![],
            x,
            y,
        }
    }

    #[test]
    fn popup_coordinates_come_together_finite_and_bounded() {
        for (x, y) in [
            (None, None),
            (Some(0.0), Some(0.0)),
            (Some(-1.0e6), Some(1.0e6)),
        ] {
            assert!(popup(x, y).check().is_ok());
        }
        for (x, y) in [(Some(0.0), None), (None, Some(-1.5))] {
            assert!(popup(x, y).check().is_err());
        }
        for value in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1.0e6 + 1.0,
            -1.0e6 - 1.0,
        ] {
            assert!(popup(Some(value), Some(0.0)).check().is_err());
            assert!(popup(Some(0.0), Some(value)).check().is_err());
        }
    }

    #[test]
    fn calls_accept_empty_lists_and_validate_nonempty_trees() {
        for items in [vec![], vec![item("a")], vec![MenuItem::default()]] {
            let expected = check_items(&items).is_ok();
            let owner = SessionId(7);
            for call in [
                MenuCall::SetApplication {
                    owner,
                    items: items.clone(),
                },
                MenuCall::SetWindow {
                    owner,
                    label: Some("other".to_owned()),
                    items: items.clone(),
                },
                MenuCall::Popup {
                    owner,
                    label: None,
                    items,
                    x: None,
                    y: None,
                },
            ] {
                assert_eq!(call.check().is_ok(), expected);
            }
        }
        assert!(MenuCall::Release {
            owner: SessionId(7)
        }
        .check()
        .is_ok());
    }
}

use console::{Style, StyledObject, strip_ansi_codes};
use dialoguer::theme::ColorfulTheme;
use mlua::prelude::*;

const FIELDS: [&str; 7] = [
    "Accent", "Color", "Error", "Muted", "Success", "Text", "Warning",
];

type Rgb = Option<[u8; 3]>;

#[derive(Debug, Clone)]
pub struct PromptColors {
    accent: Rgb,
    color: bool,
    error: Rgb,
    muted: Rgb,
    success: Rgb,
    text: Rgb,
    warning: Rgb,
}

fn rgb(table: &LuaTable, field: &str) -> LuaResult<Rgb> {
    let value = table.get::<LuaValue>(field)?;
    let LuaValue::String(value) = value else {
        return Err(LuaError::runtime(format!(
            "Prompt color field \"{field}\" must be a hex string or an empty string."
        )));
    };
    let value = value.to_str()?;
    if value.is_empty() {
        return Ok(None);
    }
    let bytes = value.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' || !bytes[1..].iter().all(u8::is_ascii_hexdigit) {
        return Err(LuaError::runtime(format!(
            "Prompt color field \"{field}\" must use \"#RRGGBB\" or an empty string."
        )));
    }
    Ok(Some([
        u8::from_str_radix(&value[1..3], 16).expect("Hex digits were validated."),
        u8::from_str_radix(&value[3..5], 16).expect("Hex digits were validated."),
        u8::from_str_radix(&value[5..7], 16).expect("Hex digits were validated."),
    ]))
}

impl FromLua for PromptColors {
    fn from_lua(value: LuaValue, _: &Lua) -> LuaResult<Self> {
        let LuaValue::Table(table) = value else {
            return Err(LuaError::runtime("Prompt colors must be a table."));
        };
        for pair in table.clone().pairs::<LuaValue, LuaValue>() {
            let (key, _) = pair?;
            let LuaValue::String(key) = key else {
                return Err(LuaError::runtime(
                    "Prompt color fields must have string keys.",
                ));
            };
            if !FIELDS.contains(&key.to_str()?.as_ref()) {
                return Err(LuaError::runtime(format!(
                    "Unknown prompt color field \"{}\".",
                    key.to_str()?
                )));
            }
        }
        let LuaValue::Boolean(color) = table.get::<LuaValue>("Color")? else {
            return Err(LuaError::runtime(
                "Prompt color field \"Color\" must be a boolean.",
            ));
        };
        Ok(Self {
            accent: rgb(&table, "Accent")?,
            color,
            error: rgb(&table, "Error")?,
            muted: rgb(&table, "Muted")?,
            success: rgb(&table, "Success")?,
            text: rgb(&table, "Text")?,
            warning: rgb(&table, "Warning")?,
        })
    }
}

impl PromptColors {
    fn style(&self, rgb: Rgb) -> Style {
        let style = Style::new().for_stderr().force_styling(self.color);
        if let Some([red, green, blue]) = rgb {
            style.true_color(red, green, blue)
        } else {
            style
        }
    }

    pub(super) fn theme(&self) -> ColorfulTheme {
        fn restyle(style: &Style, original: &StyledObject<String>) -> StyledObject<String> {
            style.apply_to(strip_ansi_codes(&original.to_string()).into_owned())
        }

        let mut theme = ColorfulTheme::default();
        let accent = self.style(self.accent);
        let error = self.style(self.error);
        let muted = self.style(self.muted);
        let success = self.style(self.success);
        let text = self.style(self.text);
        let warning = self.style(self.warning);
        theme.defaults_style = accent.clone();
        theme.prompt_style = text.clone().bold();
        theme.prompt_prefix = restyle(&warning, &theme.prompt_prefix);
        theme.prompt_suffix = restyle(&muted, &theme.prompt_suffix);
        theme.success_prefix = restyle(&success, &theme.success_prefix);
        theme.success_suffix = restyle(&muted, &theme.success_suffix);
        theme.error_prefix = restyle(&error, &theme.error_prefix);
        theme.error_style = error;
        theme.hint_style = muted.clone();
        theme.values_style = success.clone();
        theme.active_item_style = accent;
        theme.inactive_item_style = text;
        theme.active_item_prefix = restyle(&success, &theme.active_item_prefix);
        theme.inactive_item_prefix = restyle(&muted, &theme.inactive_item_prefix);
        theme.checked_item_prefix = restyle(&success, &theme.checked_item_prefix);
        theme.unchecked_item_prefix = restyle(&muted, &theme.unchecked_item_prefix);
        theme.picked_item_prefix = restyle(&success, &theme.picked_item_prefix);
        theme.unpicked_item_prefix = restyle(&muted, &theme.unpicked_item_prefix);
        theme
    }
}

#[cfg(test)]
mod tests {
    use dialoguer::theme::Theme;

    use super::*;

    fn palette(lua: &Lua, enabled: bool) -> LuaTable {
        let table = lua.create_table().unwrap();
        table.set("Color", enabled).unwrap();
        for (key, value) in [
            ("Accent", "#112233"),
            ("Error", "#223344"),
            ("Muted", "#334455"),
            ("Success", "#445566"),
            ("Text", "#556677"),
            ("Warning", "#667788"),
        ] {
            table.set(key, value).unwrap();
        }
        table
    }

    #[test]
    fn validates_every_required_color_and_rejects_unknown_fields() {
        let lua = Lua::new();
        for field in FIELDS {
            let table = palette(&lua, true);
            table.set(field, LuaValue::Nil).unwrap();
            assert!(PromptColors::from_lua(LuaValue::Table(table), &lua).is_err());
        }
        for field in FIELDS.into_iter().filter(|field| *field != "Color") {
            for invalid in ["red", "#12345", "#1234567", "#1122zz", "#é1122"] {
                let table = palette(&lua, true);
                table.set(field, invalid).unwrap();
                assert!(PromptColors::from_lua(LuaValue::Table(table), &lua).is_err());
            }
        }
        for invalid in [LuaValue::Nil, LuaValue::Boolean(true), LuaValue::Integer(1)] {
            assert!(PromptColors::from_lua(invalid, &lua).is_err());
        }
        for invalid in [
            LuaValue::Integer(1),
            LuaValue::String(lua.create_string("true").unwrap()),
        ] {
            let table = palette(&lua, true);
            table.set("Color", invalid).unwrap();
            assert!(PromptColors::from_lua(LuaValue::Table(table), &lua).is_err());
        }
        let table = palette(&lua, true);
        table.set("Unknown", "#112233").unwrap();
        assert!(PromptColors::from_lua(LuaValue::Table(table), &lua).is_err());
        let table = palette(&lua, true);
        table.set(1, "#112233").unwrap();
        assert!(PromptColors::from_lua(LuaValue::Table(table), &lua).is_err());
    }

    #[test]
    fn uses_each_explicit_rgb_slot_without_named_palette_colors() {
        let lua = Lua::new();
        let theme = PromptColors::from_lua(LuaValue::Table(palette(&lua, true)), &lua)
            .unwrap()
            .theme();
        for (style, expected) in [
            (&theme.defaults_style, "17;34;51"),
            (&theme.active_item_style, "17;34;51"),
            (&theme.error_style, "34;51;68"),
            (&theme.hint_style, "51;68;85"),
            (&theme.values_style, "68;85;102"),
            (&theme.prompt_style, "85;102;119"),
            (&theme.inactive_item_style, "85;102;119"),
        ] {
            assert!(
                style
                    .apply_to("sample")
                    .to_string()
                    .contains(&format!("\x1b[38;2;{expected}m"))
            );
        }
        for (prefix, expected) in [
            (&theme.prompt_prefix, "102;119;136"),
            (&theme.prompt_suffix, "51;68;85"),
            (&theme.success_prefix, "68;85;102"),
            (&theme.success_suffix, "51;68;85"),
            (&theme.error_prefix, "34;51;68"),
            (&theme.active_item_prefix, "68;85;102"),
            (&theme.inactive_item_prefix, "51;68;85"),
            (&theme.checked_item_prefix, "68;85;102"),
            (&theme.unchecked_item_prefix, "51;68;85"),
            (&theme.picked_item_prefix, "68;85;102"),
            (&theme.unpicked_item_prefix, "51;68;85"),
        ] {
            let rendered = prefix.to_string();
            assert!(
                rendered.contains(&format!("\x1b[38;2;{expected}m")),
                "{rendered:?}"
            );
        }
        let original = ColorfulTheme::default();
        assert_eq!(
            strip_ansi_codes(&theme.prompt_prefix.to_string()),
            strip_ansi_codes(&original.prompt_prefix.to_string())
        );
        assert_eq!(
            strip_ansi_codes(&theme.success_prefix.to_string()),
            strip_ansi_codes(&original.success_prefix.to_string())
        );
    }

    #[test]
    fn disabled_colors_remove_all_theme_styles_without_changing_global_flags() {
        let lua = Lua::new();
        let before = (console::colors_enabled(), console::colors_enabled_stderr());
        let theme = PromptColors::from_lua(LuaValue::Table(palette(&lua, false)), &lua)
            .unwrap()
            .theme();
        let mut rendered = String::new();
        theme.format_prompt(&mut rendered, "Prompt").unwrap();
        theme
            .format_input_prompt(&mut rendered, "Input", Some("Default"))
            .unwrap();
        theme
            .format_confirm_prompt(&mut rendered, "Confirm", Some(true))
            .unwrap();
        theme.format_error(&mut rendered, "Failure").unwrap();
        theme.format_select_prompt(&mut rendered, "Choose").unwrap();
        for style in [
            theme.defaults_style,
            theme.prompt_style,
            theme.error_style,
            theme.hint_style,
            theme.values_style,
            theme.active_item_style,
            theme.inactive_item_style,
        ] {
            rendered.push_str(&style.apply_to("sample").to_string());
        }
        for prefix in [
            theme.prompt_prefix,
            theme.prompt_suffix,
            theme.success_prefix,
            theme.success_suffix,
            theme.error_prefix,
            theme.active_item_prefix,
            theme.inactive_item_prefix,
            theme.checked_item_prefix,
            theme.unchecked_item_prefix,
            theme.picked_item_prefix,
            theme.unpicked_item_prefix,
        ] {
            rendered.push_str(&prefix.to_string());
        }
        assert!(!rendered.contains('\x1b'), "{rendered:?}");
        assert_eq!(
            (console::colors_enabled(), console::colors_enabled_stderr()),
            before
        );
    }

    #[test]
    fn fourth_argument_is_optional_and_validated_by_prompt_options() {
        use crate::prompt::PromptOptions;

        let lua = Lua::new();
        let kind = LuaValue::String(lua.create_string("text").unwrap());
        for fourth in [LuaValue::Nil, LuaValue::Table(palette(&lua, true))] {
            let supplied = !fourth.is_nil();
            let values =
                LuaMultiValue::from_vec(vec![kind.clone(), LuaValue::Nil, LuaValue::Nil, fourth]);
            let options = PromptOptions::from_lua_multi(values, &lua).unwrap();
            assert_eq!(options.colors.is_some(), supplied);
            assert!(options.text.is_none());
        }
        let options = PromptOptions::from_lua_multi(LuaMultiValue::new(), &lua).unwrap();
        assert!(options.colors.is_none());
        let values = LuaMultiValue::from_vec(vec![
            kind,
            LuaValue::Nil,
            LuaValue::Nil,
            LuaValue::Boolean(true),
        ]);
        assert!(PromptOptions::from_lua_multi(values, &lua).is_err());
    }

    #[test]
    fn empty_slots_leave_foregrounds_unset() {
        let lua = Lua::new();
        let table = palette(&lua, true);
        for field in FIELDS.into_iter().filter(|field| *field != "Color") {
            table.set(field, "").unwrap();
        }
        let theme = PromptColors::from_lua(LuaValue::Table(table), &lua)
            .unwrap()
            .theme();
        assert_eq!(
            theme.defaults_style.apply_to("sample").to_string(),
            "sample"
        );
        assert_eq!(
            theme.prompt_style.apply_to("sample").to_string(),
            "\x1b[1msample\x1b[0m"
        );
    }
}

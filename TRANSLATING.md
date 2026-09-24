# Translating Seecut

The editor's localization system uses English text as the key for each
translated string. A locale is a JSON file that maps those keys to another
language:

```json
{
  "_": { "name": "Deutsch" },
  "Settings": "Einstellungen",
  "Imported {0} files": "{0} Dateien importiert"
}
```

- `_` names the language in its own words for the language picker.
- Copy the keys from
  [`src/crates/concat/locales/en.json`](src/crates/concat/locales/en.json)
  and translate their values. Keep placeholders such as `{0}` and `{1}`.
- Missing keys fall back to English.

Name the file with its language code, such as `de.json`, `pt-BR.json` or
`zh-Hans.json`.

## Try a translation locally

Put the JSON file in the `locales` folder of the app's configuration directory,
then restart Seecut:

| Platform | Configuration folder |
| --- | --- |
| macOS | `~/Library/Application Support/app.concat.editor/locales/` |
| Linux | `~/.config/app.concat.editor/locales/` |
| Windows | `%APPDATA%\app.concat.editor\locales\` |

`app.concat.editor` is the configuration identifier still used by the current
code. It is retained so existing settings and translations remain accessible.
A local file with the code of a built-in language can contain only the entries
you want to override.

## Contribute a translation

1. Put the file in `src/crates/concat/locales/`.
2. Add its code and file to `BUILT_IN` in
   [`src/crates/concat/src/i18n.rs`](src/crates/concat/src/i18n.rs).
3. Run `python3 scripts/locales.py --check` from the repository root.
4. Open a pull request in the [Seecut repository](https://github.com/Stormycry-cryp/seecut).

For interface code, use `I18n.t("...")` in Slint and `t("...")` or
`tf("...", &[...])` in Rust. After adding keys, run `python3 scripts/locales.py`
to update the English inventory.

# Interface translations

`Capture Button` reads its window and tray texts from these JSON catalogs. Nothing is
compiled into the executable, so a language can be fixed or added by editing a
file.

- `ru.json` вЂ” Р СѓСЃСЃРєРёР№
- `en.json` вЂ” English (also the fallback; the keys are the English source strings)
- `zh.json` вЂ” дё­ж–‡

English is also compiled into `CaptureButton.exe`, so the portable zip ships no locale
files at all and works in English on its own. The installer puts this folder next
to `CaptureButton.exe`. A catalog in `%LOCALAPPDATA%\CaptureButton\locales` overrides the
shipped one with the same code, so a translation can be replaced without touching
Program Files; the same works for a portable copy, where the files are simply
placed next to the executable.

## Format

```json
{
  "code": "ru",
  "name": "Р СѓСЃСЃРєРёР№",
  "strings": {
    "Settings": "РќР°СЃС‚СЂРѕР№РєРё",
    "{fps} FPS": "{fps} FPS"
  }
}
```

- `code` is the file's language code and what **Settings в†’ Language** saves.
- `name` is the native name shown in that list.
- `strings` maps an English source string to its translation. A missing key
  falls back to the English text, so a partial catalog is safe.
- `{placeholders}` must be kept (they are substituted at run time); their order
  and position may change.
- Keys with several spaces, such as `"Stop capture  {time}"`, keep them: the
  spacing is part of the rendered layout.

## Applying a change

The language is chosen in **Settings в†’ Language** ("System" follows the Windows
display language). Labels are created when the window opens, so the choice
applies on the next start; the dialog says so. Adding a catalog file makes it
appear in the list immediately.


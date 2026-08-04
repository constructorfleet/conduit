/// Turning a config field's name into something a person reads.
///
/// A provider component's schema names its fields the way the JSON document
/// spells them — `base_url`, `api_key`, `threshold_percent` — because that is
/// what the API accepts. An operator filling the form in is not editing JSON,
/// though, and a form that shows them the wire spelling is asking them to
/// translate. So the wire name stays on the wire and this is what the screen
/// shows.

/// Words that are initialisms rather than words, upper-cased whole.
///
/// Written down rather than guessed at, because there is no rule that
/// distinguishes `url` from `mode`: both are three letters and only one is an
/// initialism. A field whose word is not here is title-cased, which is the
/// right answer for every ordinary word and a mild wrong answer for an
/// initialism nobody has added yet.
const INITIALISMS = new Set([
  "api",
  "cpu",
  "db",
  "gpu",
  "http",
  "https",
  "id",
  "ip",
  "json",
  "llm",
  "mcp",
  "ms",
  "pcm",
  "sse",
  "ssl",
  "stt",
  "tts",
  "url",
  "uuid",
  "wav",
]);

/// The name of `field` as an operator screen shows it.
///
/// Underscores and hyphens separate words, each word is capitalized, and a
/// word that is an initialism is upper-cased whole: `base_url` reads as
/// `Base URL` and `api_key` as `API Key`.
export function fieldLabel(field: string): string {
  const words = field
    .split(/[_\-\s]+/)
    .filter((word) => word.length > 0)
    .map((word) => {
      const lower = word.toLowerCase();
      if (INITIALISMS.has(lower)) {
        return lower.toUpperCase();
      }
      return lower.charAt(0).toUpperCase() + lower.slice(1);
    });

  // A name that is nothing but separators has no words to title-case, and an
  // empty label would leave a control with no accessible name at all. The
  // wire spelling is a worse label than a real one and a better label than
  // none.
  return words.length > 0 ? words.join(" ") : field;
}

/// The names of `fields`, joined the way a sentence lists them.
export function fieldLabels(fields: readonly string[]): string {
  return fields.map(fieldLabel).join(", ");
}

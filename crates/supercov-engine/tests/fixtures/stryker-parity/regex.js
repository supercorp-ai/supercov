// Regular expressions as weapon-regex mutates them (level 1), in literals
// and in the strings `new RegExp` is given.
export function words(text) {
  return text.match(/^\w+(?:-\w+)*$/);
}

export function quoted(text) {
  return /(?<=")[^"]*(?=")/.test(text) || /(?<!\\)'(?!')/.test(text);
}

export function letters(text) {
  return /\p{L}+|\P{N}/u.test(text) && /\p{L}/.test(text);
}

export function spaced(text) {
  return /a\v\s\S\d\D\w\W.b/.exec(text);
}

export function counted(text) {
  return /x{2,3}y*?z+?|q??/g.test(text);
}

export function sets(text) {
  return /[[a-z]--[aeiou]]/v.test(text);
}

export function dynamic(text, flags) {
  return new RegExp("^(\\d+)\\.(\\d+)$", flags).test(text) && RegExp("[^,]+").test(text);
}

export function unparseable(text) {
  return new RegExp("(?i)abc").test(text) || /()|a/.test(text);
}

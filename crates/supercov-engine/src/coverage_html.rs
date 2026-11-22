//! A static HTML report: one file, no network, no service.
//!
//! The point of this report is not a percentage -- the CLI already prints
//! that. It is to let a person trace a missing obligation to the line that
//! carries it, offline, from a CI artifact they downloaded.
//!
//! It is a single self-contained document on purpose. A copied artifact works
//! with no server and no network; there is nothing to fetch from a CDN; and
//! because a source file's path is never used as an output path, a filename
//! carrying `../` cannot write anywhere.
//!
//! Four states are kept distinct, because collapsing them into one red/green
//! score is how a report starts lying: *uncovered* (measured, nothing reached
//! it), *not applicable* (nothing eligible), *unmeasured* (Supercov declined
//! the obligation) and *stale* (the run no longer matches the checkout).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::mutation_report::{Section, SectionMutant, Tally};
use crate::run_view::{Applicability, Metric, RunView};

/// Escape text for HTML character data and quoted attribute values.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Escape a JSON document for embedding inside a `<script>` element.
///
/// A source file containing `</script>` would otherwise close the element and
/// let the rest of that file be parsed as markup -- the project's own code
/// becoming the report's HTML.
fn embed_json(value: &str) -> String {
    value
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn percentage(covered: usize, eligible: usize) -> String {
    if eligible == 0 {
        return "n/a".into();
    }
    format!("{:.2}%", covered as f64 * 100.0 / eligible as f64)
}

fn state_label(applicability: &Applicability) -> &'static str {
    match applicability {
        Applicability::Measured => "measured",
        Applicability::NotApplicable => "not applicable",
        Applicability::Incomplete { .. } => "partly measured",
    }
}

/// A score as the page shows it: `—` where there is none.
fn score(value: Option<f64>) -> String {
    value.map_or_else(
        || "\u{2014}".to_owned(),
        |value| format!("{:.1}%", value * 100.0),
    )
}

fn mutants_word(count: usize) -> &'static str {
    if count == 1 { "mutant" } else { "mutants" }
}

/// Counts as a list, the ones with nothing left out.
fn count_list(rows: &[(usize, String)]) -> String {
    let items = rows
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("<li><strong>{count}</strong> {}</li>", escape(label)))
        .collect::<String>();
    format!("<ul>{items}</ul>")
}

/// A link to a mutant's line, with what it changes and why it has its
/// outcome.
fn survivor(mutant: &SectionMutant) -> String {
    format!(
        "<li><a href=\"#{}:{}\">{}:{}</a> <code>{}</code> &middot; {} &mdash; {}</li>",
        escape(&mutant.file.replace('%', "%25").replace('#', "%23")),
        mutant.line,
        escape(&mutant.file),
        mutant.line,
        escape(&mutant.change),
        escape(mutant.outcome.name()),
        escape(&mutant.why),
    )
}

/// How many files and survivors of each the card lists before "and N more".
const SURVIVOR_FILES: usize = 5;
const SURVIVORS_EACH: usize = 3;
/// How many reasons for no verdict the card names.
const SHOWN_REASONS: usize = 4;

/// The Mutation testing card: both levels' scores and counts, why mutants
/// have no verdict, each language, and the survivors by file.
fn mutation_card(section: &Section) -> String {
    let tally = &section.tally;
    let mut card = String::from(
        "<section class=\"mutation\" aria-label=\"Mutation testing\"><h2>Mutation testing</h2>",
    );
    let languages = section
        .languages
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let _ = write!(
        card,
        "<p class=\"sub\">{} {} &middot; {} &middot; {}</p>",
        tally.mutants,
        mutants_word(tally.mutants),
        escape(&languages),
        escape(&section.mutant_set)
    );
    if let Some(diff) = &section.diff {
        let _ = write!(
            card,
            "<p class=\"note\"><strong>This change only:</strong> the mutants on {} of {} lines changed since {} (merge base {}). Mutants on other lines are left out.</p>",
            diff.lines_with_mutants,
            diff.changed_lines,
            escape(&diff.base),
            escape(&diff.merge_base.chars().take(12).collect::<String>()),
        );
    }
    if tally.mutants == 0 {
        card.push_str("<p>No mutant is on a line this change added or changed.</p></section>");
        return card;
    }
    let weak = &tally.weak;
    let decided = weak.infected + weak.not_infected + weak.not_reached;
    let _ = write!(
        card,
        "<div class=\"levels\"><div><h3>Weak mutation: {}</h3><p>infected, of {decided} of {} {}</p>{}</div>",
        score(tally.weak_score),
        tally.mutants,
        mutants_word(tally.mutants),
        count_list(&[
            (
                weak.infected,
                "infected: a test changed the mutant's value".into()
            ),
            (
                weak.not_infected,
                format!(
                    "not infected: reached, its value never changed{}",
                    if section.summary.untestable > 0 {
                        format!(
                            " ({} at a clock or random boundary)",
                            section.summary.untestable
                        )
                    } else {
                        String::new()
                    }
                )
            ),
            (weak.not_reached, "not reached: no test runs it".into()),
            (weak.unknown, "unknown: the run cannot tell".into()),
            (
                weak.equivalent,
                "equivalent: no input can tell it apart".into()
            ),
        ]),
    );
    let strong = &tally.strong;
    let counted = strong.killed + strong.timeout + strong.survived + strong.no_coverage;
    if !section.strong {
        card.push_str("<div><h3>Strong mutation: &mdash;</h3><p>No strong pass has run: no test was run against a mutant yet. <code>supercov runs &lt;run-id&gt; mutants test</code> runs each infected mutant against the tests that infected it.</p></div></div>");
    } else {
        let undriven = section
            .mutants
            .iter()
            .filter(|mutant| mutant.outcome == crate::mutation_report::Outcome::NotRun)
            .map(|mutant| {
                mutant
                    .why
                    .trim_start_matches("not run: no driver for ")
                    .trim_end_matches(" yet")
                    .to_owned()
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        let basis = match tally.mutation_score {
            Some(_) => format!(
                "killed or timed out, of {counted} of {} {}",
                tally.mutants,
                mutants_word(tally.mutants)
            ),
            None => format!(
                "No mutation score: none of the {} {} was killed, timed out, survived or left uncovered.",
                tally.mutants,
                mutants_word(tally.mutants)
            ),
        };
        let _ = write!(
            card,
            "<div><h3>Strong mutation: {}</h3><p>{}</p>{}",
            score(tally.mutation_score),
            escape(&basis),
            count_list(&[
                (strong.killed, "killed".into()),
                (strong.timeout, "timed out".into()),
                (strong.survived, "survived".into()),
                (strong.no_coverage, "no coverage".into()),
                (strong.equivalent, "equivalent".into()),
                (strong.unviable, "unviable: does not compile".into()),
                (strong.error, "error: no verdict".into()),
                (strong.not_run, format!("not run: no driver for {undriven}")),
                (strong.pending, "without a strong verdict yet".into()),
            ]),
        );
        // Why the rest have no verdict, each reason once.
        let mut reasons = BTreeMap::<&str, usize>::new();
        for mutant in &section.mutants {
            if !mutant.alive
                && matches!(
                    mutant.outcome,
                    crate::mutation_report::Outcome::Error
                        | crate::mutation_report::Outcome::NotRun
                        | crate::mutation_report::Outcome::Pending
                )
            {
                *reasons.entry(mutant.why.as_str()).or_default() += 1;
            }
        }
        if !reasons.is_empty() {
            let mut reasons = reasons
                .into_iter()
                .map(|(why, count)| (count, why.to_owned()))
                .collect::<Vec<_>>();
            reasons.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let more = reasons.len().saturating_sub(SHOWN_REASONS);
            reasons.truncate(SHOWN_REASONS);
            let _ = write!(
                card,
                "<p>Why some have no verdict:</p>{}",
                count_list(&reasons)
            );
            if more > 0 {
                let _ = write!(card, "<p>and {more} more reasons</p>");
            }
        }
        card.push_str("</div></div>");
    }
    if section.languages.len() > 1 {
        card.push_str("<table aria-label=\"Mutants by language\"><thead><tr><th scope=\"col\">Language</th><th class=\"num\" scope=\"col\">Mutants</th><th class=\"num\" scope=\"col\">Weak score</th><th class=\"num\" scope=\"col\">Mutation score</th><th class=\"num\" scope=\"col\">Left alive</th></tr></thead><tbody>");
        for (language, each) in &section.languages {
            let each: &Tally = each;
            let _ = write!(
                card,
                "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                escape(language),
                each.mutants,
                score(each.weak_score),
                score(each.mutation_score),
                each.survivors
            );
        }
        card.push_str("</tbody></table>");
    }
    // The survivors, by file: the most first, a few of each, the rest folded.
    let mut by_file = BTreeMap::<&str, Vec<&SectionMutant>>::new();
    for mutant in section.mutants.iter().filter(|mutant| mutant.alive) {
        by_file.entry(&mutant.file).or_default().push(mutant);
    }
    let mut files = by_file.into_iter().collect::<Vec<_>>();
    files.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
    let _ = write!(card, "<h3>Left alive: {}</h3>", tally.survivors);
    if files.is_empty() {
        card.push_str("<p>No mutant is left alive: every one a test can reach is killed, or has no verdict yet.</p>");
    }
    let mut rest = Vec::new();
    for (position, (file, mutants)) in files.iter().enumerate() {
        if position >= SURVIVOR_FILES {
            rest.extend(mutants.iter().copied());
            continue;
        }
        let shown = mutants
            .iter()
            .take(SURVIVORS_EACH)
            .map(|mutant| survivor(mutant))
            .collect::<String>();
        let _ = write!(
            card,
            "<p><strong>{}</strong></p><ul class=\"alive\">{shown}</ul>",
            escape(file)
        );
        rest.extend(mutants.iter().skip(SURVIVORS_EACH).copied());
    }
    if !rest.is_empty() {
        let listed = rest
            .iter()
            .map(|mutant| survivor(mutant))
            .collect::<String>();
        let _ = write!(
            card,
            "<details><summary>and {} more</summary><ul>{listed}</ul></details>",
            rest.len()
        );
    }
    card.push_str("</section>");
    card
}

const STYLE: &str = r##"
:root{--bg:#fff;--fg:#1b1b1b;--muted:#5a5a5a;--line:#d8d8d8;--hit:#e6f4ea;--miss:#fdecea;--panel:#f7f7f7;--accent:#0b5fff}
@media (prefers-color-scheme:dark){:root{--bg:#15171a;--fg:#e8e8e8;--muted:#a6a6a6;--line:#33373d;--hit:#16301f;--miss:#3a1d1b;--panel:#1c1f23;--accent:#7aa2ff}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 ui-sans-serif,system-ui,-apple-system,Segoe UI,Roboto,sans-serif}
main{max-width:72rem;margin:0 auto;padding:1.5rem}
h1{font-size:1.3rem;margin:0 0 .25rem}
.sub{color:var(--muted);margin:0 0 1.25rem}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(11rem,1fr));gap:.75rem;margin-bottom:1.5rem}
.card{border:1px solid var(--line);border-radius:.5rem;padding:.75rem;background:var(--panel)}
.card .n{font-size:1.5rem;font-weight:600}
.card .m{color:var(--muted);font-size:.85rem}
table{border-collapse:collapse;width:100%}
th,td{text-align:left;padding:.4rem .6rem;border-bottom:1px solid var(--line)}
th{cursor:pointer;user-select:none;background:var(--panel)}
th[aria-sort=ascending]::after{content:" \25B2"}
th[aria-sort=descending]::after{content:" \25BC"}
td.num,th.num{text-align:right;font-variant-numeric:tabular-nums}
a{color:var(--accent)}
.flag{font-size:.75rem;border:1px solid var(--line);border-radius:.25rem;padding:0 .3rem;color:var(--muted)}
.controls{display:flex;gap:.75rem;align-items:center;margin-bottom:.75rem;flex-wrap:wrap}
input[type=search]{padding:.35rem .5rem;border:1px solid var(--line);border-radius:.35rem;background:var(--bg);color:var(--fg);min-width:16rem}
pre{margin:0;overflow-x:auto}
.src{border:1px solid var(--line);border-radius:.5rem;overflow:hidden}
.row{display:grid;grid-template-columns:4rem 9rem 1fr;gap:0;border-bottom:1px solid var(--line)}
.row:last-child{border-bottom:0}
.row.hit{background:var(--hit)}
.row.miss{background:var(--miss)}
.row .ln{color:var(--muted);text-align:right;padding:.1rem .5rem;font-variant-numeric:tabular-nums}
.row .mk{color:var(--muted);padding:.1rem .5rem;font-size:.8rem;overflow-wrap:anywhere}
.row code{padding:.1rem .5rem;white-space:pre;font:12.5px/1.55 ui-monospace,SFMono-Regular,Menlo,monospace}
.note{border-left:3px solid var(--accent);background:var(--panel);padding:.6rem .8rem;margin:1rem 0;border-radius:0 .35rem .35rem 0}
.warn{border-left-color:#c9791a}
:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.mutation{border:1px solid var(--line);border-radius:.5rem;padding:.75rem 1rem;margin-bottom:1.5rem;background:var(--panel)}
.mutation h2{margin:.1rem 0 .5rem}
.levels{display:grid;grid-template-columns:repeat(auto-fit,minmax(16rem,1fr));gap:1rem}
.levels h3{font-size:1rem;margin:.25rem 0}
.levels ul,.mutation ul{margin:.25rem 0;padding-left:1.1rem}
.alive{color:#b44f00}
@media (prefers-color-scheme:dark){.alive{color:#ffa452}}
.mutants{grid-column:1/-1;padding:.3rem .6rem .5rem 4.5rem;overflow-wrap:anywhere;background:var(--panel);font-size:.85rem}
.mutants div{margin:.25rem 0;padding-left:.5rem;border-left:3px solid var(--line)}
.mutants div.alive{border-left-color:#b44f00;color:inherit}
.mutants code{font:12px ui-monospace,SFMono-Regular,Menlo,monospace}
"##;

const SCRIPT: &str = r##"
const data = JSON.parse(document.getElementById('supercov-data').textContent);
const files = data.files;
const escape = (value) => String(value).replace(/[&<>"']/g, (c) =>
  ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const pct = (c, e) => e === 0 ? 'n/a' : (c * 100 / e).toFixed(2) + '%';
const metric = (file, name) => (file.metrics || []).find((m) => m.metric === name);
const mutation = data.mutants;
// A file's mutation score: the strong one once a pass gave verdicts, the weak
// one before; null where the file has none.
const mutantTally = (path) => mutation && mutation.files[path] || null;
const mutantScore = (tally) => !tally ? null : mutation.strong && tally.mutationScore !== null ? tally.mutationScore : tally.weakScore;
const outcomeNames = {killed:'killed', timeout:'timed out', survived:'survived', 'no-coverage':'no coverage', equivalent:'equivalent', unviable:'unviable', error:'error', 'not-run':'not run', pending:'no strong verdict'};
let sort = { key: 'file', dir: 1 };

function rows() {
  const term = document.getElementById('filter').value.trim().toLowerCase();
  const onlyGaps = document.getElementById('gaps').checked;
  let shown = files.filter((f) => f.file.toLowerCase().includes(term));
  if (onlyGaps) shown = shown.filter((f) => f.uncoveredLines.length || f.missingBranches.length || f.missingConditions.length);
  shown.sort((a, b) => {
    const pick = (f) => {
      if (sort.key === 'file') return f.file;
      if (sort.key === 'mutants') { const score = mutantScore(mutantTally(f.file)); return score === null ? 2 : score; }
      const m = metric(f, sort.key);
      return m && m.eligible ? m.covered / m.eligible : -1;
    };
    const x = pick(a), y = pick(b);
    return (x > y ? 1 : x < y ? -1 : 0) * sort.dir;
  });
  document.getElementById('rows').innerHTML = shown.map((f) => {
    const cell = (name) => {
      const m = metric(f, name);
      if (!m || m.eligible === 0) return '<td class="num"><span class="flag">n/a</span></td>';
      return `<td class="num">${pct(m.covered, m.eligible)} <span class="flag">${m.covered}/${m.eligible}</span></td>`;
    };
    const mutantCell = () => {
      if (!mutation) return '';
      const tally = mutantTally(f.file);
      if (!tally) return '<td class="num"><span class="flag">none</span></td>';
      const score = mutantScore(tally);
      return `<td class="num" title="${tally.survivors} of ${tally.mutants} left alive">${score === null ? '\u2014' : (score * 100).toFixed(1) + '%'} <span class="flag${tally.survivors ? ' alive' : ''}">${tally.survivors} alive</span></td>`;
    };
    return `<tr><td><a href="#${encodeURIComponent(f.file)}" data-file="${escape(f.file)}">${escape(f.file)}</a></td>`
      + cell('lines') + cell('branches') + cell('mcdc') + mutantCell() + '</tr>';
  }).join('') || `<tr><td colspan="${mutation ? 5 : 4}">No files match.</td></tr>`;
  document.getElementById('count').textContent = `${shown.length} of ${files.length} file(s)`;
}

function showFile(path) {
  const file = files.find((f) => f.file === path);
  const panel = document.getElementById('source');
  if (!file) { panel.innerHTML = ''; return; }
  const uncovered = new Set(file.uncoveredLines);
  const measured = new Set(file.measuredLines);
  const branchAt = {}, conditionAt = {};
  for (const at of file.missingBranches) branchAt[at.line] = (branchAt[at.line] || 0) + 1;
  for (const at of file.missingConditions) conditionAt[at.line] = (conditionAt[at.line] || 0) + 1;
  const text = data.sources[path];
  // Each line's mutants: what they change, both levels, and the test that
  // killed one; a mutant the tests leave alive is set apart.
  const mutantsAt = {};
  for (const m of (mutation ? mutation.mutants : []).filter((m) => m.file === path)) (mutantsAt[m.line] = mutantsAt[m.line] || []).push(m);
  const mutantBlock = (number) => {
    const here = mutantsAt[number];
    if (!here) return '';
    return '<div class="mutants">' + here.map((m) => `<div class="${m.alive ? 'alive' : ''}"><strong>${m.alive ? 'Left alive \u00b7 ' : ''}${escape(m.mutator)}</strong> <code>${escape(m.change)}</code><br>`
      + `Weak: ${escape(m.status.replace('-', ' '))} \u2014 ${escape(m.reason)}`
      + (m.outcome === 'pending' && !mutation.strong ? '' : `<br>Strong: ${['pending', 'not-run'].includes(m.outcome) ? '' : escape(outcomeNames[m.outcome] || m.outcome) + ' \u2014 '}${escape(m.why)}`)
      + (m.killedBy ? `<br>Killed by ${escape(m.killedBy)}` : '') + '</div>').join('') + '</div>';
  };
  let body;
  if (typeof text !== 'string') {
    body = `<p class="note warn">Source is not embedded for this file, so only line numbers are shown. ${escape(data.sourceReason || '')}</p>`
      + '<ul>' + file.uncoveredLines.map((l) => `<li>line ${l} not covered</li>`).join('') + '</ul>'
      + Object.keys(mutantsAt).map((number) => `<p>Line ${number}</p>` + mutantBlock(Number(number))).join('');
  } else {
    body = '<div class="src">' + text.split('\n').map((line, index) => {
      const number = index + 1;
      const marks = [];
      if (measured.has(number)) marks.push(uncovered.has(number) ? 'not covered' : 'covered');
      if (branchAt[number]) marks.push(`${branchAt[number]} branch gap`);
      if (conditionAt[number]) marks.push(`${conditionAt[number]} MC/DC gap`);
      const here = mutantsAt[number] || [];
      if (here.length) marks.push(`${here.length} mutant${here.length === 1 ? '' : 's'}, ${here.filter((m) => m.alive).length} alive`);
      const cls = !measured.has(number) ? '' : uncovered.has(number) ? 'miss' : 'hit';
      const mark = !measured.has(number) ? '' : uncovered.has(number) ? '\u2717' : '\u2713';
      return `<div class="row ${cls}" id="${encodeURIComponent(path)}:${number}">`
        + `<span class="ln">${number}</span>`
        + `<span class="mk">${mark ? mark + ' ' : ''}${escape(marks.join(', '))}</span>`
        + `<code>${escape(line) || '&nbsp;'}</code></div>` + mutantBlock(number);
    }).join('') + '</div>';
  }
  panel.innerHTML = `<h2>${escape(path)}</h2>` + body;
  panel.scrollIntoView({ block: 'start' });
}

function route() {
  const hash = decodeURIComponent(location.hash.replace(/^#/, ''));
  const [path, line] = hash.split(/:(?=\d+$)/);
  if (path) showFile(path);
  // A survivor's link goes to its line.
  const row = line && document.getElementById(`${encodeURIComponent(path)}:${line}`);
  if (row) row.scrollIntoView({ block: 'center' });
}

document.getElementById('filter').addEventListener('input', rows);
document.getElementById('gaps').addEventListener('change', rows);
for (const th of document.querySelectorAll('th[data-key]')) {
  const apply = () => {
    const key = th.dataset.key;
    sort = { key, dir: sort.key === key ? -sort.dir : 1 };
    for (const other of document.querySelectorAll('th[data-key]')) other.removeAttribute('aria-sort');
    th.setAttribute('aria-sort', sort.dir === 1 ? 'ascending' : 'descending');
    rows();
  };
  th.addEventListener('click', apply);
  th.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); apply(); }
  });
}
// Open the file directly as well as through the hash. A deep link still
// works, but the report must not depend on hash navigation behaving the same
// way in every context an artifact gets opened from.
document.getElementById('rows').addEventListener('click', (event) => {
  const link = event.target.closest('a[data-file]');
  if (!link) return;
  event.preventDefault();
  const path = link.dataset.file;
  history.replaceState(null, '', '#' + encodeURIComponent(path));
  showFile(path);
});
window.addEventListener('hashchange', route);
rows();
route();
"##;

/// Render the report. `sources` carries file text only when it was verified to
/// match the run; a file absent from it is shown by line number instead of
/// annotated against source that may have moved.
pub fn html(
    view: &RunView,
    sources: &BTreeMap<String, String>,
    source_reason: &str,
    mutants: Option<&Section>,
) -> String {
    let payload = serde_json::json!({
        "files": view.files,
        "sources": sources,
        "sourceReason": source_reason,
        "mutants": mutants,
    });
    let mutation = mutants.map(mutation_card).unwrap_or_default();
    let mutants_head = if mutants.is_some() {
        "\n<th tabindex=\"0\" data-key=\"mutants\" class=\"num\" scope=\"col\">Mutants</th>"
    } else {
        ""
    };
    let payload = embed_json(&serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into()));

    let mut cards = String::new();
    Metric::ALL.into_iter().for_each(|metric| {
        let counts = view.metric(metric);
        let _ = write!(
            cards,
            "<div class=\"card\"><div class=\"n\">{}</div><div class=\"m\">{} &middot; {}/{} &middot; {}</div></div>",
            escape(&percentage(counts.covered, counts.eligible)),
            escape(metric.name()),
            counts.covered,
            counts.eligible,
            escape(state_label(&counts.applicability)),
        );
    });

    let mut notes = String::new();
    if !view.suite_passed {
        notes.push_str("<p class=\"note warn\"><strong>The test command did not pass.</strong> These numbers describe the run that failed; they are not a statement that the project is covered.</p>");
    }
    if view.stale {
        let _ = write!(
            notes,
            "<p class=\"note warn\"><strong>This run no longer matches the checkout.</strong> {}</p>",
            escape(&view.stale_reasons.join(", "))
        );
    }
    if !view.complete {
        notes.push_str("<p class=\"note\">Some obligations were not measured exactly. They are excluded from every count here, because a measurement gap is not a coverage gap.</p>");
    }
    for limitation in &view.limitations {
        let _ = write!(notes, "<p class=\"note\">{}</p>", escape(limitation));
    }

    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Coverage {run}</title>
<style>{STYLE}</style>
</head>
<body>
<main>
<h1>Coverage report</h1>
<p class="sub">Run {run} &middot; recorded {generated}</p>
{notes}
<div class="cards">{cards}</div>
{mutation}<h2>Files</h2>
<div class="controls">
  <label for="filter">Filter files</label>
  <input id="filter" type="search" placeholder="path contains&hellip;">
  <label><input id="gaps" type="checkbox"> Only files with gaps</label>
  <span id="count" class="flag" aria-live="polite"></span>
</div>
<table>
<thead><tr>
<th tabindex="0" data-key="file" aria-sort="ascending" scope="col">File</th>
<th tabindex="0" data-key="lines" class="num" scope="col">Lines</th>
<th tabindex="0" data-key="branches" class="num" scope="col">Branches</th>
<th tabindex="0" data-key="mcdc" class="num" scope="col">MC/DC</th>{mutants_head}
</tr></thead>
<tbody id="rows"></tbody>
</table>
<section id="source" aria-live="polite"></section>
</main>
<script id="supercov-data" type="application/json">{payload}</script>
<script>{SCRIPT}</script>
</body>
</html>
"##,
        run = escape(&view.run),
        generated = escape(&view.generated_at),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_view::{FileView, MetricView, RUN_VIEW_SCHEMA_VERSION, RunView};
    use std::collections::BTreeSet;

    #[test]
    fn text_is_escaped_and_each_state_named() {
        assert_eq!(escape(""), "");
        assert_eq!(escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&#39;f");
        assert_eq!(state_label(&Applicability::Measured), "measured");
        assert_eq!(state_label(&Applicability::NotApplicable), "not applicable");
        assert_eq!(
            state_label(&Applicability::Incomplete { unmeasured: 1 }),
            "partly measured"
        );
    }

    /// A run's limitations are noted on the page, each escaped.
    #[test]
    fn a_run_s_limitations_are_noted() {
        let mut limited = view();
        limited.limitations = vec!["half <measured>".into()];
        let page = html(&limited, &BTreeMap::new(), "", None);
        assert!(
            page.contains("<p class=\"note\">half &lt;measured&gt;</p>"),
            "{page}"
        );
    }

    fn view() -> RunView {
        RunView {
            schema_version: RUN_VIEW_SCHEMA_VERSION,
            run: "run_1".into(),
            generated_at: "now".into(),
            suite_passed: true,
            stale: false,
            stale_reasons: Vec::new(),
            complete: true,
            limitations: Vec::new(),
            totals: vec![
                MetricView {
                    metric: Metric::Lines,
                    covered: 1,
                    eligible: 2,
                    applicability: Applicability::Measured,
                },
                MetricView {
                    metric: Metric::Mcdc,
                    covered: 0,
                    eligible: 0,
                    applicability: Applicability::NotApplicable,
                },
            ]
            .into_iter()
            .chain(
                [Metric::Statements, Metric::Functions, Metric::Branches].map(|metric| {
                    MetricView {
                        metric,
                        covered: 0,
                        eligible: 0,
                        applicability: Applicability::NotApplicable,
                    }
                }),
            )
            .collect(),
            files: vec![FileView {
                file: "src/<script>.ts".into(),
                metrics: vec![MetricView {
                    metric: Metric::Lines,
                    covered: 1,
                    eligible: 2,
                    applicability: Applicability::Measured,
                }],
                measured_lines: vec![1, 2],
                uncovered_lines: vec![2],
                missing_branches: Vec::new(),
                missing_conditions: Vec::new(),
                functions: Vec::new(),
                branches: Vec::new(),
                conditions: Vec::new(),
                points: Vec::new(),
                branch_sites: Vec::new(),
            }],
            source_neighbourhoods: BTreeSet::new(),
        }
    }

    #[test]
    fn project_source_can_never_become_the_report_s_own_markup() {
        // A file containing `</script>` would otherwise close the data island
        // and have the rest of the project's code parsed as HTML.
        let mut sources = BTreeMap::new();
        sources.insert(
            "src/<script>.ts".to_owned(),
            "const a = '</script><img src=x onerror=alert(1)>'\n".to_owned(),
        );
        let text = html(&view(), &sources, "", None);
        let island = text
            .split("<script id=\"supercov-data\" type=\"application/json\">")
            .nth(1)
            .and_then(|rest| rest.split("</script>").next())
            .expect("data island");
        assert!(
            !island.contains("</script"),
            "the island can be closed early"
        );
        assert!(
            !island.contains('<') && !island.contains('>'),
            "raw markup survived"
        );
        assert!(island.contains("\\u003c/script\\u003e"), "{island}");
        // The filename reaches the visible document only escaped.
        let body = text.split("<script id=").next().expect("body");
        assert!(
            !body.contains("src/<script>"),
            "an unescaped filename reached the page"
        );
    }

    #[test]
    fn the_report_is_self_contained_and_fetches_nothing() {
        // It has to open from a copied CI artifact with no network at all.
        // The check is on the document, not the whole file: a project's own
        // source legitimately contains URLs, and embedding it must not make
        // this test pass or fail for the wrong reason.
        let mut sources = BTreeMap::new();
        sources.insert(
            "src/<script>.ts".to_owned(),
            "const endpoint = 'https://example.test/api'\n".to_owned(),
        );
        let text = html(&view(), &sources, "", None);
        let island = "<script id=\"supercov-data\" type=\"application/json\">";
        let start = text.find(island).expect("data island");
        let end = text[start..].find("</script>").expect("island end") + start;
        let document = format!("{}{}", &text[..start], &text[end..]);
        for remote in [
            "src=\"http",
            "href=\"http",
            "<link",
            "@import",
            "//cdn",
            "fetch(",
            "XMLHttpRequest",
        ] {
            assert!(
                !document.contains(remote),
                "{remote} appears in the document"
            );
        }
        assert!(text.contains("<style>") && text.contains("<script>"));
    }

    #[test]
    fn distinct_states_are_never_collapsed_into_one_score() {
        // "nothing eligible" must not read as complete, and a failed or stale
        // run must say so beside its numbers rather than presenting them as a
        // verdict on the project.
        let text = html(&view(), &BTreeMap::new(), "", None);
        assert!(text.contains("not applicable"), "{text}");
        assert!(text.contains("measured"));

        let mut broken = view();
        broken.suite_passed = false;
        broken.stale = true;
        broken.stale_reasons = vec!["instrumented source changed".into()];
        broken.complete = false;
        let text = html(&broken, &BTreeMap::new(), "", None);
        assert!(text.contains("The test command did not pass"));
        assert!(text.contains("no longer matches the checkout"));
        assert!(text.contains("instrumented source changed"));
        assert!(text.contains("a measurement gap is not a coverage gap"));
    }

    #[test]
    fn coverage_is_marked_by_glyph_and_words_not_colour_alone() {
        // Colour cannot be the only carrier: the source view labels each line
        // covered or not covered in text, with a tick or cross beside it.
        assert!(SCRIPT.contains("'covered'") && SCRIPT.contains("'not covered'"));
        assert!(SCRIPT.contains("\\u2713") && SCRIPT.contains("\\u2717"));
        // Sortable headers are reachable and announce their state.
        assert!(STYLE.contains(":focus-visible"));
        let text = html(&view(), &BTreeMap::new(), "", None);
        assert!(text.contains("tabindex=\"0\"") && text.contains("aria-sort"));
        assert!(STYLE.contains("prefers-color-scheme:dark"));
    }

    fn mutant(
        file: &str,
        line: u32,
        alive: bool,
        outcome: crate::mutation_report::Outcome,
    ) -> SectionMutant {
        SectionMutant {
            id: format!("{file}:{line}"),
            file: file.into(),
            line,
            column: 1,
            end_line: line,
            mutator: "EqualityOperator".into(),
            change: "a <= b → a < b".into(),
            status: crate::coverage_index::MutantStatus::Infected,
            reason: "weak".into(),
            code: "c".into(),
            untestable: None,
            outcome,
            killed_by: None,
            tests: None,
            duration_ms: None,
            why: match outcome {
                crate::mutation_report::Outcome::NotRun => {
                    "not run: no driver for cobol yet".into()
                }
                _ => format!("why {line}"),
            },
            alive,
        }
    }

    fn section(mutants: Vec<SectionMutant>, strong: bool) -> Section {
        let mut tally = Tally {
            mutants: mutants.len(),
            survivors: mutants.iter().filter(|mutant| mutant.alive).count(),
            weak_score: Some(0.5),
            mutation_score: strong.then_some(0.25),
            ..Tally::default()
        };
        tally.weak.infected = mutants.len();
        tally.weak.not_infected = 1;
        tally.strong.killed = 1;
        tally.strong.not_run = 1;
        let mut summary = crate::mutants_query::MutantsSummary::of(tally.weak.clone(), 1);
        summary.untestable = 1;
        Section {
            mutant_set: "set@1".into(),
            summary,
            tally: tally.clone(),
            strong,
            languages: BTreeMap::from([
                ("javascript".into(), tally.clone()),
                ("python".into(), Tally::default()),
            ]),
            diff: None,
            files: BTreeMap::from([("src/a.js".into(), tally)]),
            mutants,
        }
    }

    /// The Mutation testing card: both levels, why some have no verdict,
    /// each language, the survivors by file with the rest folded, and the
    /// change it was narrowed to; the files' table gains a Mutants column.
    #[test]
    fn the_mutation_card_shows_both_levels_survivors_and_the_change() {
        use crate::mutation_report::Outcome::*;
        let mut mutants = Vec::new();
        for file in 0..6 {
            for line in 1..=4 {
                mutants.push(mutant(&format!("src/f{file}.js"), line, true, Survived));
            }
        }
        mutants.push(mutant("src/a.js", 9, false, NotRun));
        mutants.push(mutant("src/a.js", 10, false, Pending));
        mutants.push(mutant("src/a.js", 11, false, Pending));
        mutants.push(mutant("src/a.js", 12, false, Pending));
        mutants.push(mutant("src/a.js", 13, false, Error));
        let strong = section(mutants, true);
        let page = html(&view(), &BTreeMap::new(), "", Some(&strong));
        for expected in [
            "<h2>Mutation testing</h2>",
            "29 mutants &middot; javascript, python &middot; set@1",
            "Weak mutation: 50.0%",
            "(1 at a clock or random boundary)",
            "Strong mutation: 25.0%",
            "<strong>1</strong> not run: no driver for cobol</li>",
            "Why some have no verdict:",
            "<strong>1</strong> not run: no driver for cobol yet</li>",
            "<p>and 1 more reasons</p>",
            "aria-label=\"Mutants by language\"",
            "<h3>Left alive: 24</h3>",
            "<a href=\"#src/f0.js:1\">src/f0.js:1</a>",
            "<details><summary>and 9 more</summary>",
            "data-key=\"mutants\"",
        ] {
            assert!(page.contains(expected), "{expected} in {page}");
        }
        // No strong pass: says so; nothing alive: says that; a change.
        let mut weak = section(vec![mutant("src/a.js", 1, false, Pending)], false);
        weak.diff = Some(crate::mutants_query::DiffSummary {
            base: "main".into(),
            merge_base: "0123456789abcdef".into(),
            commit: None,
            changed_lines: 4,
            lines_with_mutants: 1,
        });
        let page = html(&view(), &BTreeMap::new(), "", Some(&weak));
        for expected in [
            "No strong pass has run",
            "No mutant is left alive",
            "<strong>This change only:</strong> the mutants on 1 of 4 lines changed since main (merge base 0123456789ab)",
        ] {
            assert!(page.contains(expected), "{expected} in {page}");
        }
        // A pass with no verdict a score counts; a change with no mutants.
        let mut unscored = section(vec![mutant("src/a.js", 1, false, Error)], true);
        unscored.tally.mutation_score = None;
        unscored.languages.remove("python");
        let page = html(&view(), &BTreeMap::new(), "", Some(&unscored));
        assert!(
            page.contains("No mutation score: none of the 1 mutant was killed"),
            "{page}"
        );
        assert!(!page.contains("Mutants by language"), "{page}");
        let mut empty = section(Vec::new(), false);
        empty.diff = weak.diff.clone();
        let page = html(&view(), &BTreeMap::new(), "", Some(&empty));
        assert!(
            page.contains("No mutant is on a line this change added or changed."),
            "{page}"
        );
        // No mutants: no card and no column.
        let page = html(&view(), &BTreeMap::new(), "", None);
        assert!(!page.contains("Mutation testing") && !page.contains("data-key=\"mutants\""));
        assert_eq!(score(None), "\u{2014}");
        assert_eq!(mutants_word(1), "mutant");
    }
}

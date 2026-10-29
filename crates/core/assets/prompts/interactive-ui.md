## Interactive output capability (this client)

This terminal renders a native, bounded OpenUI-style component language directly
in your reply. Write a `nexus-ui` fence containing assignments and component
calls, NOT JSON, HTML or JavaScript. Use root-first composition so elements and
text appear while you are still generating the rest:

- `root = Panel("Short title", [heading, content, controls], "rounded", "accent2")`
- `heading = Hero("A clear headline", "One useful supporting sentence", "OPTIONAL BADGE")`
- Define the remaining named components in reading order. Forward references
  are supported; only components reachable from root are shown. No empty holes
  are reserved for references that have not arrived yet.
- Arguments are positional, based on the terminal component signatures below.
  Omit optional trailing arguments; `null` skips an optional argument in the middle.
  Calls and arrays may span lines. Strings use double or single quotes; prefer
  double quotes with standard escapes. Comments start with `//` or `#`.
- Close the fence when done. Never print the component source outside the fence.

Click inline or use F2, Tab/arrows, Enter. Inputs accept typing, Backspace and
paste. Prefer interactive UI for games, quizzes, calculators, choices and forms; ordinary
answers use Markdown. This is declarative data, not executable code.

For apps, build state → controls → derived displays → actions. Use two control
kinds; answer-only lists are for quizzes/polls. UI first, minimal prose, shallow navigation.

### State, expressions and actions

Declare local state with `$days = 5`, `$ready = false`, `$name = ""`.
Declare it immediately after root and before the visible components that use it.
Initial state may use `@Random([1, 2, 3])`; it is chosen once when the completed
panel initializes. State is temporary and resets on session/space changes.

Bindable arguments take `$name`, not a quoted key:
- `Stepper($days, "Days", 1, 30, 1)`: Left/Right adjusts and clamps a number.
- `Input($name, "Your name", {required: true, email: true})`: string input;
  never passwords/API keys. Optional rules: required/email/url/numeric booleans,
  minLength/maxLength, min/max, pattern. Empty values skip all but required.
  Pattern subset: anchored `^...$`, ASCII literals, `.`, `[a-z]`, `\\d \\w \\s`,
  `* + ?`; no groups/alternation. Escape backslashes in strings. Errors appear
  inline and block actions reading invalid fields; sends validate every input.
- `Select($mode, "Mode", ["Fast", "Full"])`: cycle a string choice.
- `Toggle($ready, "Ready")`: flip a boolean.
- `Checklist([Check("Beach", $beach), Check("Hiking", $hiking)])`.
- `Table(["Name"], [["Ada"]], $row)`: optional numeric row selection.
- `Editor($code, "Solution", "python", 8)`: multi-line code editor, 3–16 rows;
  Enter starts editing, Esc stops. `$code` holds starter code (spaces, no tabs).
- `Choice($pick, "Pick one", ["Hash map", "List"], "Hash map")`: radio list;
  `$pick = ""` until picked. The optional answer marks right/wrong once picked.

Numeric expressions support `$state`, literals, named references, parentheses,
`+ - * / %`, comparisons `== != < > <= >=`, booleans `&& || !`, and
`condition ? yes : no` (inside a call or on one line). Conditions are booleans;
arithmetic takes numbers. `@Number($amount)` converts a numeric string.
`@If(condition, yes, no)` is equivalent to the conditional expression.
Use expressions in Progress, Bars, Sparkline and @Set. They recompute locally.
Text arguments accept literal text, `$state`, or concatenation such as
`Text("Days away: " + $days)`. `"{{days}} days"` also substitutes state.
For arithmetic results use progress/bars, not arithmetic inside a text string.
Data supports nested `{key: value}` objects, arrays, `a.field`, `a[i]`, `.length`;
`rows.price` plucks each price (missing fields/indexes give null).
Built-ins work in expressions/text: `@Count @First @Last @Sum @Avg @Min @Max`
(array), `@Abs @Floor @Ceil` (number), `@Round(number, decimals?)`.
`@Filter(array, "field.path", "==", value)` accepts == != > < >= <= contains.
`@Sort(array, "field.path", "desc")` is stable; default ascending. Empty field
means each element itself. Numeric built-ins coerce numeric strings.
`@Each(array, "row", Text(row.name))` returns a reactive list; use it as a list
argument or within one. The variable is template-local;
no index variable. ≤200 iterations; ordinary node/item caps still apply.

A Button takes an explicit action:
- `Button("Add", @Set($count, $count + 1), true)`: local change; `true` marks
  the primary action. Use one primary and at most one secondary for simple apps.
- `@Reset($days, $ready)` restores declared defaults.
- `@OpenUrl("https://example.com")` asks for Enter confirmation in the panel;
  Esc cancels. Only HTTPS without credentials; opens the OS browser, logs status.
- `Button("Save plan", @ToAssistant("Plan " + $days + " days", {days: $days}), true)`:
  marked ↵; prepares a response in an EMPTY composer for the user to review/edit
  and press Enter to send. Message + optional JSON context + current state/form
  values go together. Existing drafts are never overwritten.
- `Button("B — Paris", @Answer())`: marked →; sends the clicked rendered label
  verbatim after the reply finishes, without a second review Enter. Use equal
  answer buttons for quizzes, polls and follow-up choices.
- `Action([@Set($saved, true), @ToAssistant("Saved")])`: bounded sequential,
  transactional steps. At most one message per action. Named action references
  are allowed. There are no arbitrary event handlers or function definitions.

New elements appear progressively but remain inert until the fence closes and
this reply finishes. Older local panels still work during generation. No model
requests occur for local-only panels. Live data runs only after activation. The renderer
validates arriving elements and the completed program automatically. Stream
directly; no preflight tool call is needed. Correct any reported validation
errors and resend the panel. Never invent components/functions.
History labels programs with ids (p2-1). Patch via a nexus-ui fence starting
`edit p2-1`, then ONLY changed assignments. Names replace/append; `name = null`
deletes; unreachable values are pruned. Completed patches validate atomically,
retain surviving state, or leave the panel unchanged with an error. No legacy
JSON edits. Last-reply errors return once.
The user can pin a panel as a mini-app (key p). To change one, use the panel_app
tool: list, read, then patch with the same changed assignments (no edit line).
Patch only when asked.

### Composition and limits

Panel frame: rounded (default), double, heavy, dashed, plain. Accent: accent,
accent2 (default), success, warning, error, muted. These are theme roles, not colors.
Tone: normal, muted, accent, success, warning, error. Mark: ok, no, dot, none.
Step state: done, current, todo. Callout tone: info, tip, warning, danger.

Use Hero for hierarchy, Stats for key numbers, Cards for 1–6 summaries, Columns
for 2–3 comparisons, Steps for timelines, and Callout for one useful takeaway.
A Button may include body lines for 3–8 carousel choices: artwork, title, detail.
Image/src arguments must be real HTTPS URLs copied exactly from search mode
"images" or tool results, never invented or templated. Include alt text. Pictures
reserve fixed height and may load late/fail; text must stand alone.

Tabs: Tab(label, view) children; arrows/click switch, optional numeric key.
Accordion: Section(title, view, open?) children; Enter/click expands.
Chart: line|area|scatter|bar|pie; Series samples, labels and theme tones.
Table: sortable?, page_size? (1–8), search?; s/arrows sort, PgUp/PgDn page, / filter.
Form: bound fields; submit action reads current state; inputs failing their rules block it.
Timeline: Event(time, title, text?, tone?) entries.
Rating: numeric stars; arrows/click edit.
Code: literal source, optional language, Copy button.
Badge: compact themed text. Meter: themed gauge.
Slider: numeric range; arrows/click adjust by step.
Quiz: Question(prompt, view?, answer?, explain?) children, one shown at a time;
arrows page, Show answer reveals, header counts right Choice answers. For
interview/practice sets put Editor/Choice/Input in each question and end with
`Button("Grade my answers", @ToAssistant("Grade my answers"), true)`: all state,
including code, goes with it.

Source ≤16 KiB; ≤128 named statements; ≤16 visible nodes; ≤32 state fields,
32 controls/actions and 128 action steps. Interactive-only panels have 21 declared
node rows; display layouts have 45. Text and buttons declare 1–8 rows; text wraps
at actual width. Image rows 4–12. Lists are bounded: buttons/checks/bars/steps ≤8,
cards/stats ≤6, table ≤6 columns (8 rows, or 64 with pagination), sparkline/chart ≤64 samples. Text and editor code ≤4096
bytes; other scalar strings ≤1024; titles/labels/art lines ≤120; identifiers ≤32 ASCII
letters/digits/underscores; finite numbers ≤1e12. Keep UIs compact.

Data arrays/objects ≤200 entries, depth ≤12, bounded evaluation work.
Live data: Query(tool, args, defaults, refreshSeconds?) runs after reply completion
AND user activation; literal $vars in args refetch after 300ms. Refresh ≥15s, ≤5
per panel; pauses off-screen/unfocused, stops after 30min idle. Defaults/last data
stay visible. __openui_loading.q, __openui_refetching.q, __openui_errors.q expose
query q's status. Query allow-list (only if advertised and read-only): search,
fetch_url, files, fs, canvas, connectors and exposed connector tools. Search, files
search and connectors may cost: user must press Load first. No shell/proc/task/
batch/media or LLM queries. Mutation(tool,args) runs only via @Run(ref); writes
always ask approval. Value: {status,data,error}; idle/loading/success/error.
@Run(query) refetches. Calls are capped, timed out and logged as tool cards.
Never put credentials in args.
No arbitrary functions or browser APIs other than confirmed OpenUrl.
Existing version-1 JSON panels remain readable, but do not generate new JSON.

Example app (state → controls → computed progress → reviewed plan):
```nexus-ui
root = Panel("Trip planner", [days, activities, planned, save])
$days = 5
$beach = true
$hiking = false
days = Stepper($days, "Days", 1, 30, 1)
activities = Checklist([Check("Beach (2 days)", $beach), Check("Hiking (3 days)", $hiking)])
planned = Progress("Planned days", @If($beach, 2, 0) + @If($hiking, 3, 0), $days)
save = Buttons([Button("Save plan", @ToAssistant("Plan {{days}} days: beach={{beach}}, hiking={{hiking}}"), true)])
```

Live-data example:
```nexus-ui
root = Panel("Search", [Input($query,"Search"), Text(q.text), Buttons([Button("Refresh",@Run(q))])])
$query = "Rust"
q = Query("search", {mode:"web",query:$query}, {text:"No results"})
```

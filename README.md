# QuickQL

QuickQL is a VS Code extension for running `.ql` files against JSON and CSV data with a Rust query engine and Rust language server.

## Query Format

QuickQL queries are line-based pipelines. Blank lines are ignored, and `--`
starts a comment.

```ql
SOURCE 'example.json'
FILTER count = 7
MAP count, search
SORT_BY count DESC
```

`SOURCE` reads one or more source files. Relative paths are resolved from the
`.ql` file, and quotes around paths are optional. Separate multiple sources
with commas to append their rows:

```ql
SOURCE 'example.csv'
```

```ql
SOURCE 'one.csv', 'two.csv'
```

HTTP JSON sources are loaded with a GET request. Add request headers after the
source with `HEADERS`:

```ql
SOURCE 'https://api.example.test/users' HEADERS Authorization = 'Bearer token'
```

JSON sources may be a single object or an array of objects. QuickQL uses the
top-level object keys as columns. CSV sources must include headers; comma,
semicolon, and tab delimiters are detected from the first row.

`MAP` keeps columns in the listed order. Use `MAP *`, or omit `MAP`,
to return every column. Rename columns with `output=input`, or add quoted
static string columns with `output="value"`. It can also add computed columns
with `GETDATE`, which maps an ISO timestamp like `2026-05-26T18:23:07.004Z` to
`2026-05-26`:

```ql
SOURCE 'example.csv'
MAP length=count, search, text="test... "
```

```ql
SOURCE 'example.json'
MAP *, date = GETDATE(updatedAt)
```

`FILTER` currently supports equality filters. Use `OR` to match any of several
filters on the same line:

```ql
SOURCE 'example.json'
FILTER index = global_doku_de OR index = public_docs
FILTER published = true
MAP count, day, search
```

Quoted string values work too:

```ql
SOURCE 'example.csv'
FILTER Channel = 'Public Knowledge Base'
MAP Number_of_Searches, Search_Term
```

`GROUP_BY` deduplicates rows by one or more key columns. It can also compute
aggregations with `SUM`, `ARRAY`, `COUNT`, `MINDATE`, and `MAXDATE`. Date
aggregations parse strings formatted as `23.01.2026`, `23-01-2026`, or
`2026-01-23`:

```ql
SOURCE 'example.csv'
FILTER Channel = 'Public Knowledge Base'
GROUP_BY Search_Term MAP Number_of_Searches = SUM(Number_of_Searches), Search_Dates = ARRAY(Search_Date), Count = COUNT(Search_Term), First_Date = MINDATE(Search_Date), Last_Date = MAXDATE(Search_Date)
SORT_BY Number_of_Searches DESC
```

Use `GROUP_BY *` to aggregate every input row into a single group:

```ql
SOURCE 'example.json'
GROUP_BY * MAP ids = ARRAY(id)
```

For JSON object columns, use dot paths to group or aggregate nested values:

```ql
SOURCE 'example.json'
GROUP_BY metadata.source MAP count = COUNT(id)
```

`SORT_BY` accepts one or more columns. Direction is optional and defaults to
ascending:

```ql
SOURCE 'example.json'
SORT_BY count DESC, search ASC
```

`MAP_MANY` expands an array column into one row per array item. This is useful
for paged HTTP responses like `{ "items": [...], "totalCount": 10299 }`:

```ql
SOURCE 'https://api.example.test/users' HEADERS Authorization = 'Bearer token'
MAP *
MAP_MANY items
```

Supported query lines are `SOURCE`, `FILTER`, `MAP`, `MAP_MANY`, `GROUP_BY`,
and `SORT_BY`. Lines run in the order they appear, so filter, map, or group
before a `MAP` that removes columns needed by later steps. Multiple `FILTER`
lines are applied as separate pipeline steps. `LIMIT` and `AND` are not
currently implemented.

## Usage

Open a `.ql` file and press the play button in the editor title or the CodeLens above the query.

Results open in the bottom QuickQL panel. The Rust engine streams result rows to the extension process over stdout, and the extension keeps paged rows in memory for the virtualized table. No result file is written for normal VS Code query execution.

## Completions

The language server suggests QuickQL keywords and reads the JSON or CSV file
referenced by `SOURCE` to suggest source field names.

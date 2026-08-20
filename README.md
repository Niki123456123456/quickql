# QuickQL (quick query language)

A lightweight pipeline query language for transforming JSON, CSV, and HTTP data sources. QuickQL queries are plain text files (`.ql`) that describe a sequence of data transformation steps executed top to bottom.

## Quick Example

```ql
SOURCE OPEN('orders.json')
FILTER EQ(status, 'shipped')
MAP customer_id, total, shipped_date = GETDATE(shipped_at)
GROUP_BY customer_id MAP total = SUM(total), last_ship = MAXDATE(shipped_date)
SORT_BY last_ship DESC
LIMIT 1024
```

## Statements

Each line in a `.ql` file is one pipeline step. Steps are separated by newlines; comments start with `--`.

| Statement | Description |
|-----------|-------------|
| [`SOURCE`](docs/source.md) | Load data from a file, URL, or inline literal |
| [`MAP`](docs/map.md) | Select, rename, or compute columns |
| [`FILTER`](docs/filter.md) | Keep only rows matching a condition |
| [`MAP_MANY`](docs/map_many.md) | Flatten an array field into individual rows |
| [`GROUP_BY`](docs/group_by.md) | Group rows by keys and aggregate |
| [`SORT_BY`](docs/sort_by.md) | Sort rows by one or more columns |
| [`LIMIT`](docs/limit.md) | Keep at most the first number of rows |

## Data Sources

QuickQL can read:

- **JSON files** — array of objects or a single object/array
- **CSV files** — automatically detected by `.csv` extension
- **HTTP endpoints** — `GET`, `POST`, or `PUT` with optional headers, body, and pagination
- **Other `.ql` files** — compose queries by referencing them as sources

```ql
-- JSON file
SOURCE OPEN('data/users.json')

-- CSV file
SOURCE OPEN('reports/sales.csv')

-- HTTP API
SOURCE GET('https://api.example.com/users')

-- Another query
SOURCE OPEN('other_query.ql')

-- Multiple sources merged
SOURCE OPEN('users.json'), OPEN('admins.json')
```

## Transforming Data

### Select and rename columns

```ql
SOURCE OPEN('users.json')
MAP id, name, email
```

```ql
SOURCE OPEN('users.json')
MAP id, full_name = name, contact = email
```

### Compute new fields

```ql
SOURCE OPEN('orders.json')
MAP *, total_with_tax = SUM(total, tax)
```

### Filter rows

```ql
SOURCE OPEN('users.json')
FILTER active
```

```ql
SOURCE OPEN('orders.json')
FILTER AND(EQ(status, 'pending'), total)
```

### Flatten nested arrays

```ql
SOURCE OPEN('invoices.json')  -- each invoice has a "lines" array
MAP_MANY lines
MAP product_id, quantity, price
```

### Group and aggregate

```ql
SOURCE OPEN('sales.json')
GROUP_BY region MAP revenue = SUM(amount), orders = COUNT(amount)
```

### Sort

```ql
SOURCE OPEN('products.json')
SORT_BY price DESC, name ASC
```

### Limit

```ql
SOURCE OPEN('products.json')
SORT_BY price DESC
LIMIT 1024
```

## Functions

| Function | Description |
|----------|-------------|
| `SUM(field)` | Sum of numeric values (works on grouped arrays) |
| `COUNT(field)` | Count of values |
| `ARRAY(a, b, ...)` | Collect values into an array |
| `UNZIPROWS(rows)` | Convert row objects into column arrays |
| `CONCAT(a, b, ...)` | Concatenate strings |
| `INDEXOF(array, value)` | Zero-based index of a value in an array, or `-1` |
| `EQ(a, b)` | `true` if `a` equals `b` |
| `AND(a, b, ...)` | `true` if all arguments are truthy |
| `OR(a, b, ...)` | `true` if any argument is truthy |
| `GETDATE(field)` | Extract the date part from an ISO datetime string |
| `ISODATE(field)` | Convert a date like `24.03.2026` to `2026-03-24` |
| `MINDATE(field)` | Earliest date in a set |
| `MAXDATE(field)` | Latest date in a set |
| `BASE64(value)` | Base64-encode a value |
| `COLOR(index)` | Deterministic RGB color for a zero-based index |
| `OPTICS(matrix, config)` | Run OPTICS cluster analysis over a numeric matrix |
| `OPEN(src)` / `GET(src)` | Load a file or URL (HTTP GET) |
| `POST(src)` | HTTP POST |
| `PUT(src)` | HTTP PUT |

See [docs/functions.md](docs/functions.md) for full details and examples.

## Values and Expressions

- **Field reference**: `name`, `address.city` (dot-notation for nested fields)
- **Secret reference**: `@API_TOKEN` (resolved at runtime)
- **String**: `'hello'` or `"hello"`
- **Number**: `42`, `-3.14`
- **Boolean**: `true`, `false`
- **Inline object**: `{key: value, other: 'text'}`
- **Inline array**: `[1, 2, 3]`
- **Function call**: `SUM(amount)`

See [docs/values-and-expressions.md](docs/values-and-expressions.md) for full details.

When a query is run from the VS Code extension, secret references are loaded from
the current process environment or a `.env` file in the query's workspace folder.
Process environment values take precedence over `.env` values.

## Development

**Install locally**

```sh
code --install-extension quickql-0.0.3.vsix
```

**Build package**

```sh
vsce package
```

## Detailed Documentation

- [SOURCE](docs/source.md) — loading data
- [MAP](docs/map.md) — transforming and selecting columns
- [FILTER](docs/filter.md) — filtering rows
- [MAP_MANY](docs/map_many.md) — flattening nested arrays
- [GROUP_BY](docs/group_by.md) — grouping and aggregation
- [SORT_BY](docs/sort_by.md) — sorting results
- [LIMIT](docs/limit.md) — limiting the number of results
- [Functions](docs/functions.md) — built-in functions reference
- [Values & Expressions](docs/values-and-expressions.md) — literals, references, operators

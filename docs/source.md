# SOURCE

Loads data into the pipeline. `SOURCE` must be the first statement in a query and can load from JSON files, CSV files, other `.ql` files, or HTTP endpoints. Multiple sources are merged into a single row set.

## Syntax

```
SOURCE value [, value ...]
```

`value` must be a function call that loads data — `OPEN()`, `GET()`, `POST()`, or `PUT()`. Bare strings are not loaded as files; they must be passed into one of these functions.

---

## File Sources

File paths are resolved relative to the `.ql` file. Use `OPEN()` to load a file.

### JSON file

```ql
SOURCE OPEN('data/orders.json')
```

The file can be:
- An array of objects — each element becomes a row
- A single object — treated as one row
- A nested object where an inner array is flattened later with `MAP_MANY`

### CSV file

Detected automatically by the `.csv` extension.

```ql
SOURCE OPEN('reports/sales.csv')
```

Each CSV row becomes an object with keys derived from the header row.

### Another .ql file

Compose queries by referencing another `.ql` file. QuickQL executes it and uses its output as rows. Circular references are detected and rejected.

```ql
-- base query: users_active.ql
SOURCE OPEN('users.json')
FILTER active
```

```ql
-- downstream query
SOURCE OPEN('users_active.ql')
MAP id, email
SORT_BY email
```

---

## HTTP Sources

Use `GET`, `POST`, or `PUT` to load data from an HTTP endpoint. `OPEN` and `GET` are equivalent.

### Simple GET

```ql
SOURCE GET('https://api.example.com/users')
```

### GET with headers

Pass a config object with `src` and `headers`:

```ql
SOURCE GET({
  src: 'https://api.example.com/users',
  headers: {Authorization: 'Bearer mytoken'}
})
```

### POST with body

```ql
SOURCE POST({
  src: 'https://api.example.com/search',
  headers: {Content-Type: 'application/json'},
  body: {query: 'active', limit: 100}
})
```

---

## Pagination

For APIs that page results, add a `paging` key to the config object.

### Cursor-based pagination

Reads a cursor token from the response body and sends it back as a query parameter on the next request. Continues until the cursor is `null` or missing.

```ql
SOURCE GET({
  src: 'https://api.example.com/items',
  paging: {
    type: 'cursor',
    in:   {location: 'query', path: 'cursor'},
    from: {location: 'body',  path: 'paging.next'}
  }
})
```

- `in` — where to send the cursor on subsequent requests (`query` or `body`)
- `from` — where to read the next cursor from in the response body (dot-path)

### Offset-based pagination

Increments an offset parameter until a page returns fewer rows than `pagesize`.

```ql
SOURCE GET({
  src: 'https://api.example.com/items',
  paging: {
    type:     'offset',
    in:       {location: 'query', path: 'offset'},
    path:     'items',
    pagesize: 50
  }
})
```

- `in` — the query or body parameter to set to the current offset
- `path` — dot-path to the array in the response used to count entries per page
- `pagesize` — expected entries per full page; stops when a page has fewer

---

## Multiple Sources

List multiple sources separated by commas. All rows are merged into one set.

```ql
SOURCE OPEN('users_eu.json'), OPEN('users_us.json')
```

```ql
SOURCE GET('https://api.example.com/page1'), GET('https://api.example.com/page2')
```

---

## Inline Data

You can source inline JSON objects or arrays directly for testing or static lookups:

```ql
SOURCE [{id: 1, name: 'Alice'}, {id: 2, name: 'Bob'}]
```

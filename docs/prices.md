# Usage estimates

`crossagents usage` and `usage_get` show reported cost separately from estimates.
To enable estimates, create `prices.toml` in the CrossAgents home directory. Model
names must match exactly. Prices are US dollars per million tokens:

```toml
["provider/model-name"]
input = 1.0
cached_input = 0.1
cache_write_input = 1.25
output = 5.0
```

These numbers illustrate the format, not actual prices. The ordinary input price
applies to input excluding cache reads and cache writes. An estimate is unknown
when a session has no token usage or any contributing model has no price.

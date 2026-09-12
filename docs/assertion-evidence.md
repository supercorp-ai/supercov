# Assertion evidence

The earlier automatic JS/TS analyzer has been replaced by agent-authored
assertion maps. Use [Understanding assertion coverage](assertions.md) to get
started, [Agent-authored assertion maps](assertion-maps.md) for the file format
and CLI reference, and [the agent instructions](assertion-agent.md) to build or
update a map. Existing structural runs remain readable; mapping requires a new
run with frozen assertion inputs. Old `--pragmas`, `--analysis` and `--evidence`
options do not apply to the map workflow.

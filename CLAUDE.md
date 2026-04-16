# AFT - Agent File Tools

Tree-sitter powered code analysis for massive context savings (60-90% token reduction).

## AFT CLI Commands

Use `aft` commands via Bash for code navigation. These provide structured output optimized for LLM consumption.

### Semantic Commands (prefer these over raw file reads)

```bash
# Get structure without content (~10% of full read tokens)
aft outline <file|directory>

# Inspect symbol with call-graph annotations
aft zoom <file> <symbol>

# Forward call graph - what does this function call?
aft call_tree <file> <symbol>

# Reverse call graph - who calls this function?
aft callers <file> <symbol>

# Impact analysis - what breaks if this changes?
aft impact <file> <symbol>

# Trace analysis - how does execution reach this?
aft trace_to <file> <symbol>
```

### Basic Commands

```bash
aft read <file> [start_line] [limit]   # Read with line numbers
aft grep <pattern> [path]              # Trigram-indexed search
aft glob <pattern> [path]              # File pattern matching
```

## When to Use What

| Task | Command | Token Savings |
|------|---------|---------------|
| Understanding file structure | `aft outline` | ~90% vs full read |
| Finding function definition | `aft zoom file symbol` | Exact code only |
| Understanding dependencies | `aft call_tree` | Structured graph |
| Finding usage sites | `aft callers` | All call sites |
| Planning refactors | `aft impact` | Change propagation |
| Debugging call paths | `aft trace_to` | Execution paths |

## Best Practices

1. **Start with outline** - Before reading a file, use `aft outline` to understand structure
2. **Zoom to symbols** - Instead of reading full files, use `aft zoom` for specific functions
3. **Use call graphs** - For understanding code flow, `call_tree` and `callers` are more efficient than grep
4. **Impact before refactor** - Run `aft impact` before making changes to understand blast radius

## Supported Languages

TypeScript, JavaScript, Python, Rust, Go, C/C++, Java, Ruby, Markdown

## Hook Integration

Read, Grep, and Glob tools are automatically routed through AFT via hooks for indexed performance.

import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";

const z = tool.schema;

/**
 * Tool definitions for scope-aware structure commands:
 * add_member, add_derive, wrap_try_catch, add_decorator, add_struct_tags.
 */
export function structureTools(ctx: PluginContext): Record<string, ToolDefinition> {
  return {
    aft_transform: {
      description:
        "Scope-aware structural code transformations with correct indentation.\n" +
        "Ops:\n" +
        "- 'add_member': Insert method/field into class, struct, or impl block. Needs 'scope' (target container) and 'code'. Optional 'position' (first/last/before:name/after:name).\n" +
        "- 'add_derive': Add Rust derive macros to struct/enum. Needs 'target' and 'derives' array. Deduplicates.\n" +
        "- 'wrap_try_catch': Wrap a TS/JS function body in try/catch. Needs 'target' (function name). Optional 'catch_body'.\n" +
        "- 'add_decorator': Add Python decorator to function/class. Needs 'target' and 'decorator' (without @). Optional 'position' (first/last).\n" +
        "- 'add_struct_tags': Add/update Go struct field tags. Needs 'target' (struct), 'field', 'tag', 'value'.\n" +
        "All ops need 'file'. Returns formatted, validation_errors.",
      args: {
        op: z
          .enum(["add_member", "add_derive", "wrap_try_catch", "add_decorator", "add_struct_tags"])
          .describe("Transformation operation"),
        file: z.string().describe("Path to the source file"),
        // add_member
        scope: z
          .string()
          .optional()
          .describe("Container name to insert into (add_member — class, struct, or impl block)"),
        code: z.string().optional().describe("Member code to insert (add_member)"),
        position: z
          .string()
          .optional()
          .describe(
            "Insert position: 'first', 'last' (default), 'before:name', 'after:name' (add_member, add_decorator)",
          ),
        // add_derive, wrap_try_catch, add_decorator, add_struct_tags
        target: z
          .string()
          .optional()
          .describe(
            "Target symbol name (add_derive: struct/enum, wrap_try_catch: function, add_decorator: function/class, add_struct_tags: struct)",
          ),
        derives: z
          .array(z.string())
          .optional()
          .describe("Derive macro names (add_derive — e.g. ['Clone', 'Debug'])"),
        catch_body: z
          .string()
          .optional()
          .describe("Catch block body (wrap_try_catch — default: 'throw error;')"),
        decorator: z
          .string()
          .optional()
          .describe("Decorator text without @ (add_decorator — e.g. 'staticmethod')"),
        // add_struct_tags
        field: z.string().optional().describe("Struct field name (add_struct_tags)"),
        tag: z.string().optional().describe("Tag key (add_struct_tags — e.g. 'json')"),
        value: z
          .string()
          .optional()
          .describe("Tag value (add_struct_tags — e.g. 'user_name,omitempty')"),
        // common
        validate: z
          .enum(["syntax", "full"])
          .optional()
          .describe("Validation level: 'syntax' (default) or 'full'"),
        dry_run: z.boolean().optional().describe("Preview without modifying the file"),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        const op = args.op as string;
        const params: Record<string, unknown> = { file: args.file };
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;

        switch (op) {
          case "add_member":
            params.scope = args.scope;
            params.code = args.code;
            if (args.position !== undefined) params.position = args.position;
            break;
          case "add_derive":
            params.target = args.target;
            params.derives = args.derives;
            break;
          case "wrap_try_catch":
            params.target = args.target;
            if (args.catch_body !== undefined) params.catch_body = args.catch_body;
            break;
          case "add_decorator":
            params.target = args.target;
            params.decorator = args.decorator;
            if (args.position !== undefined) params.position = args.position;
            break;
          case "add_struct_tags":
            params.target = args.target;
            params.field = args.field;
            params.tag = args.tag;
            params.value = args.value;
            break;
        }

        const response = await bridge.send(op, params);
        return JSON.stringify(response);
      },
    },
  };
}

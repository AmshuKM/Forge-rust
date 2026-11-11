You build one small Rust app for Forge. Follow the output format exactly.

The owner asked for this app, named {{name}}:
<request>
{{idea}}
</request>

How Forge apps work:
<guide>
{{guide}}
</guide>

{{examples}}

Files so far:
{{files}}

{{failure}}

Write these three files: apps/{{name}}/src/main.rs (with at least one unit test), apps/{{name}}/app.json and
apps/{{name}}/evals/golden.jsonl (at least 5 cases). Use only the forge_app crate and Rust's std. The app must never panic.

Output format, with nothing before or after it:
CAUSE: one sentence describing the app you built
FILE: path/of/file
<<<
the complete content of that file
>>>
Repeat the FILE block for each file.

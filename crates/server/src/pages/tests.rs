//! Page TSX parser and compiler: an accept/reject corpus, including
//! attempts to sneak code, styles and event handlers into a page.

use super::compile::{Compiled, Resolver, compile};
use super::spec::{Value, action};

/// A Space page in `s-team` (rooms `general`, `releases`; agents `agent`,
/// `ci-bot`), or a personal page whose author is a member of `s-team` only.
struct Fake {
    space: Option<&'static str>,
}

impl Resolver for Fake {
    fn page_space(&self) -> Option<&str> {
        self.space
    }
    fn space(&mut self, id: &str) -> Result<String, String> {
        if id == "s-team" {
            Ok("Team".to_owned())
        } else {
            Err(format!("no Space {id} that you're a member of"))
        }
    }
    fn room(&mut self, space: &str, name: &str) -> Result<(String, String), String> {
        assert_eq!(space, "s-team");
        match name.to_ascii_lowercase().as_str() {
            "general" => Ok(("r-general".into(), "general".into())),
            "releases" => Ok(("r-releases".into(), "releases".into())),
            _ => Err(format!("no room named \"{name}\" in this Space")),
        }
    }
    fn agent(&mut self, space: &str, handle: &str) -> Result<(String, String), String> {
        assert_eq!(space, "s-team");
        match handle {
            "agent" => Ok(("ag-1".into(), "agent".into())),
            "ci-bot" => Ok(("ag-2".into(), "ci-bot".into())),
            _ => Err(format!("no agent @{handle} in this Space")),
        }
    }
}

fn space_page(src: &str) -> Compiled {
    compile(
        src,
        &mut Fake {
            space: Some("s-team"),
        },
    )
}

fn personal_page(src: &str) -> Compiled {
    compile(src, &mut Fake { space: None })
}

fn page(body: &str) -> String {
    format!("export default (\n  <Page title=\"T\">\n{body}\n  </Page>\n);\n")
}

#[track_caller]
fn ok(c: &Compiled) {
    assert!(
        c.diagnostics.is_empty() && c.spec.is_some(),
        "expected success, got {:#?}",
        c.diagnostics
    );
    invariants(c.spec.as_ref().unwrap());
}

/// What every accepted spec must satisfy: it reads back from its own JSON,
/// and every data read starts at a declared `<Data>` or an enclosing `as`.
#[track_caller]
fn invariants(spec: &super::spec::PageSpec) {
    let json = serde_json::to_string(spec).unwrap();
    let back: super::spec::PageSpec = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("an accepted spec doesn't read back: {e}"));
    assert_eq!(&back, spec);
    fn roots(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Lit { .. } => {}
            Value::Bind { path } => out.push(path[0].clone()),
            Value::Not { a } => roots(a, out),
            Value::Coalesce { a, b } => {
                roots(a, out);
                roots(b, out);
            }
            Value::Cmp { a, .. } => roots(a, out),
        }
    }
    fn walk(b: &super::spec::Block, scope: &mut Vec<String>) {
        let mut reads = Vec::new();
        for v in b.props.values().chain(&b.content) {
            roots(v, &mut reads);
        }
        for r in reads {
            assert!(scope.contains(&r), "`{r}` read without a declaration");
        }
        let pushed = match b.props.get("as") {
            Some(Value::Lit { v }) => {
                scope.push(v.as_str().unwrap().to_owned());
                true
            }
            _ => false,
        };
        for c in &b.children {
            walk(c, scope);
        }
        if pushed {
            scope.pop();
        }
    }
    let mut scope: Vec<String> = spec.data.iter().map(|d| d.name.clone()).collect();
    for b in &spec.blocks {
        walk(b, &mut scope);
    }
}

#[track_caller]
fn rejects(c: &Compiled, needle: &str) {
    assert!(c.spec.is_none(), "expected rejection containing {needle:?}");
    assert!(
        c.diagnostics.iter().any(|d| d.message.contains(needle)),
        "no diagnostic containing {needle:?}: {:#?}",
        c.diagnostics
    );
}

const RELEASE_WATCH: &str = r#"// A dashboard.
export default (
  <Page title="Release watch" refresh="5m">
    <Data name="ci" source="agentAnswer" room="releases" agent="ci-bot" />
    <Data name="runs" source="runs" room="releases" limit={10} />
    <Data name="files" source="library" query="release" limit={5} />

    <Row key="top">
      <Stat key="active" label="Active runs" value={runs.active} />
      <Stat key="last" label="Last answer" value={ci.at} format="relative" />
    </Row>
    <Section key="answer" title="CI summary">
      <AgentAnswer key="ci" from={ci} empty="No answer yet" />
      <Button key="ask" label="Ask for a new summary"
              action="runAgent" agent="ci-bot" room="releases"
              prompt="Summarise the CI status of the release branch." />
    </Section>
    <Table key="runs" rows={runs.items} empty="Nothing running">
      <Column field="agent" label="Agent" />
      <Column field="status" label="Status" format="status" />
      <Column field="started" label="Started" format="relative" />
    </Table>
    {/* a JSX comment */}
    <Show key="busy" when={runs.active > 0}>
      <Text key="busy-text" tone="warning">Busy: {runs.active} runs &amp; counting</Text>
    </Show>
    <List key="list" items={runs.items} as="run" empty="none">
      <Badge key="b" tone="info">{run.status}</Badge>
    </List>
    <FileList key="files" from={files} />
  </Page>
);
"#;

#[test]
fn the_design_example_compiles_to_a_resolved_spec() {
    let c = space_page(RELEASE_WATCH);
    ok(&c);
    let spec = c.spec.unwrap();
    assert_eq!(spec.title, "Release watch");
    assert_eq!(spec.refresh, "5m");
    assert_eq!(spec.data.len(), 3);
    let ci = &spec.data[0];
    assert_eq!(ci.source, "agentAnswer");
    assert_eq!(ci.params.room_id.as_deref(), Some("r-releases"));
    assert_eq!(ci.params.agent_id.as_deref(), Some("ag-2"));
    // A Space page's own Space is implied, never stored per source.
    assert_eq!(ci.params.space_id, None);
    assert_eq!(spec.data[1].params.limit, Some(10));
    let ask = spec.block("ask").unwrap();
    let a = ask.action.as_ref().unwrap();
    assert_eq!(a.kind, action::RUN_AGENT);
    assert_eq!(a.target.agent_id.as_deref(), Some("ag-2"));
    assert_eq!(a.target.room_id.as_deref(), Some("r-releases"));
    assert_eq!(
        a.prompt.as_deref(),
        Some("Summarise the CI status of the release branch.")
    );
    // Action parameters live in the action, not the props.
    assert!(!ask.props.contains_key("prompt"));
    let text = spec.block("busy-text").unwrap();
    assert_eq!(text.content.len(), 3);
    assert_eq!(
        text.content[2],
        Value::Lit {
            v: " runs & counting".into()
        }
    );
    assert_eq!(spec.server_actions().len(), 1);
    // The source had every key: it is stored unchanged.
    assert_eq!(c.source, RELEASE_WATCH);
}

#[test]
fn missing_keys_are_added_deterministically_and_recompile_the_same() {
    let src = page(
        r#"    <Row>
      <Stat label="A" value={1} />
      <Stat label="B" value={2} />
      <Stat key="stat-1" label="C" value={3} />
    </Row>"#,
    );
    let c = space_page(&src);
    ok(&c);
    assert!(c.source.contains("<Row key=\"row-1\">"), "{}", c.source);
    // `stat-1` is taken by the explicit key, so the first gets stat-2.
    assert!(
        c.source.contains("<Stat key=\"stat-2\" label=\"A\""),
        "{}",
        c.source
    );
    assert!(
        c.source.contains("<Stat key=\"stat-3\" label=\"B\""),
        "{}",
        c.source
    );
    let again = space_page(&c.source);
    ok(&again);
    assert_eq!(again.source, c.source);
    assert_eq!(again.spec, c.spec);
    assert_eq!(space_page(&src).source, c.source, "deterministic");
}

#[test]
fn accepted_variants() {
    for body in [
        // Bare boolean, numbers, null literals, nested layout.
        r#"<Section key="s" title="x" collapsed><Grid key="g" columns={3}><Divider key="d" /></Grid></Section>"#,
        r#"<Heading key="h" level={2}>Hello</Heading>"#,
        r#"<Heading key="h" level="3">Hello</Heading>"#,
        r#"<Markdown key="m">**bold** text</Markdown>"#,
        r##"<Markdown key="m">{"# Title\n\nbody"}</Markdown>"##,
        r#"<Progress key="p" value={0.5} label="Half" tone="success" />"#,
        r#"<Stat key="s" label='single quotes' value={null} />"#,
        r#"<Stat key="s" label="neg" value={-2.5e3} />"#,
        r#"<Text key="t">{`plain template`}</Text>"#,
        r#"<Empty key="e">Nothing here</Empty>"#,
        r#"<Button key="r" label="Refresh" action="refresh" />"#,
        r#"<Link key="l" label="Open" action="openRoom" room="general" />"#,
        r#"<Link key="l" label="Other page" action="openPage" page="pg_0123456789abcdef0123456789abcdef" />"#,
        r#"<Button key="s" label="Run" action="runSchedule" schedule="sch_0123456789abcdef0123456789abcdef" />"#,
    ] {
        ok(&space_page(&page(body)));
    }
    // Conditions.
    for when in [
        "{!runs.active}",
        "{runs.active ?? 0}",
        "{runs.items.0.status === \"running\"}",
        "{runs.active !== 0}",
        "{(runs.active) >= 2}",
        "{!!runs.active}",
        "{runs?.active}",
    ] {
        let body = format!(
            "<Data name=\"runs\" source=\"runs\" />\n<Show key=\"s\" when={when}><Divider key=\"d\" /></Show>"
        );
        ok(&space_page(&page(&body)));
    }
    // Without parentheses or semicolon, with comments around.
    ok(&space_page(
        "/* hi */ export default <Page title=\"x\"><Divider key=\"d\"/></Page> // end",
    ));
}

#[test]
fn code_is_rejected_with_a_reason() {
    let cases: &[(&str, &str)] = &[
        // Functions and calls.
        (
            r#"<Stat key="s" label="x" value={() => 1} />"#,
            "functions aren't allowed",
        ),
        (
            r#"<Stat key="s" label="x" value={(a) => a} />"#,
            "functions aren't allowed",
        ),
        (
            r#"<Stat key="s" label="x" value={function () { return 1 }} />"#,
            "functions aren't allowed",
        ),
        (
            r#"<Data name="runs" source="runs" /><Stat key="s" label="x" value={runs.items.map(x)} />"#,
            "calls like runs.items.map(…) aren't allowed",
        ),
        (
            r#"<Stat key="s" label="x" value={fetch("https://evil.example")} />"#,
            "calls like fetch(…)",
        ),
        (
            r#"<Stat key="s" label="x" value={eval("1")} />"#,
            "calls like eval(…)",
        ),
        (
            r#"<Stat key="s" label="x" value={new Date()} />"#,
            "constructors aren't allowed",
        ),
        (
            r#"<Stat key="s" label="x" value={import("x")} />"#,
            "imports aren't allowed",
        ),
        (
            r#"<Stat key="s" label="x" value={require("fs")} />"#,
            "imports aren't allowed",
        ),
        (
            r#"<Stat key="s" label="x" value={await x} />"#,
            "async code",
        ),
        (
            r#"<Stat key="s" label="x" value={this.x} />"#,
            "`this` isn't allowed",
        ),
        // Expressions the language lacks.
        (
            r#"<Data name="runs" source="runs" /><Stat key="s" label="x" value={runs.active + 1} />"#,
            "arithmetic",
        ),
        (
            r#"<Data name="runs" source="runs" /><Stat key="s" label="x" value={runs.active ? 1 : 2} />"#,
            "?: isn't allowed",
        ),
        (
            r#"<Data name="runs" source="runs" /><Show key="s" when={runs.active && runs.items}><Divider key="d"/></Show>"#,
            "&& and || aren't allowed",
        ),
        (
            r#"<Data name="runs" source="runs" /><Show key="s" when={runs.active == 1}><Divider key="d"/></Show>"#,
            "use === or !==",
        ),
        (
            r#"<Data name="runs" source="runs" /><Show key="s" when={runs.active > runs.items}><Divider key="d"/></Show>"#,
            "compare against a literal",
        ),
        (
            r#"<Data name="runs" source="runs" /><Stat key="s" label="x" value={runs.active ?? 0} />"#,
            "only allowed in <Show when",
        ),
        (
            r#"<Data name="runs" source="runs" /><Stat key="s" label="x" value={runs["active"]} />"#,
            "computed access",
        ),
        (
            r#"<Data name="runs" source="runs" /><Stat key="s" label="x" value={runs.__proto__} />"#,
            "`__proto__` can't be read",
        ),
        (
            r#"<Data name="runs" source="runs" /><Stat key="s" label="x" value={runs.constructor} />"#,
            "`constructor` can't be read",
        ),
        (
            r#"<Stat key="s" label="x" value={window.location} />"#,
            "unknown name `window`",
        ),
        (
            r#"<Stat key="s" label="x" value={globalThis} />"#,
            "unknown name `globalThis`",
        ),
        (
            r#"<Stat key="s" label="x" value={x = 1} />"#,
            "assignments aren't allowed",
        ),
        (
            r#"<Stat key="s" label="x" value={[1, 2]} />"#,
            "array literals",
        ),
        (
            r#"<Stat key="s" label="x" value={{a: 1}} />"#,
            "object literals",
        ),
        (
            r#"<Stat key="s" label="x" value={`a${b}`} />"#,
            "template strings with ${…}",
        ),
        (
            r#"<Stat key="s" label="x" value={tag`x`} />"#,
            "tagged templates",
        ),
        (
            r#"<Stat key="s" label="x" value={undefined} />"#,
            "`undefined` isn't allowed",
        ),
        (
            r#"<Stat key="s" label="x" value={1, 2} />"#,
            "comma expressions",
        ),
        (
            r#"<Stat key="s" label="x" value={typeof x} />"#,
            "operators other than",
        ),
        // Spreads, JSX-in-expressions, fragments.
        (r#"<Stat key="s" {...props} />"#, "spread attributes"),
        (r#"<Stat key="s" label="x" value={...x} />"#, "spread"),
        (
            r#"<Section key="s">{cond && <Text key="t">x</Text>}</Section>"#,
            "<Show when={…}> to show blocks conditionally",
        ),
        (
            r#"<Section key="s">{<Text key="t">x</Text>}</Section>"#,
            "elements inside {…} aren't allowed",
        ),
        (
            r#"<Section key="s">{x.map(i => <Text key="t">x</Text>)}</Section>"#,
            "aren't allowed",
        ),
        (r#"<><Divider key="d"/></>"#, "fragments"),
        // Event handlers, style and HTML injection.
        (
            r#"<Button key="b" label="x" action="refresh" onClick={go} />"#,
            "event handlers aren't allowed",
        ),
        (
            r#"<Button key="b" label="x" action="refresh" onClick="alert(1)" />"#,
            "event handlers aren't allowed",
        ),
        (
            r#"<Section key="s" onMouseOver="x"><Divider key="d"/></Section>"#,
            "event handlers aren't allowed",
        ),
        (
            r#"<Text key="t" style="color:red">x</Text>"#,
            "styling properties aren't allowed",
        ),
        (
            r#"<Text key="t" style={{color: "red"}}>x</Text>"#,
            "object literals",
        ),
        (
            r#"<Text key="t" className="evil">x</Text>"#,
            "styling properties aren't allowed",
        ),
        (
            r#"<Text key="t" dangerouslySetInnerHTML="x">x</Text>"#,
            "raw HTML isn't allowed",
        ),
        (r#"<div>hi</div>"#, "HTML elements like <div>"),
        (
            r#"<script>alert(1)</script>"#,
            "HTML elements like <script>",
        ),
        (r#"<style>body{}</style>"#, "HTML elements like <style>"),
        (r#"<iframe src="x" />"#, "HTML elements like <iframe>"),
        (
            r#"<Text key="t" aria-label="x">x</Text>"#,
            "dashed properties",
        ),
        (r#"<Text key="t" xlink:href="x">x</Text>"#, "namespaced"),
        (r#"<svg:a />"#, "namespaced or custom tags"),
        (r#"<Foo.Bar />"#, "member tags"),
        (r#"<WebView key="w" />"#, "unknown block <WebView>"),
        (r#"<Html key="w" />"#, "unknown block <Html>"),
        // Text gotchas.
        (r#"<Text key="t">a } b</Text>"#, "`}` in text"),
        (r#"<Text key="t">a > b</Text>"#, "`>` in text"),
    ];
    for (body, needle) in cases {
        let src = page(body);
        let c = space_page(&src);
        assert!(c.spec.is_none(), "accepted: {body}");
        assert!(
            c.diagnostics.iter().any(|d| d.message.contains(needle)),
            "{body}: wanted {needle:?}, got {:#?}",
            c.diagnostics
        );
    }
}

#[test]
fn program_shape_is_enforced() {
    let cases: &[(&str, &str)] = &[
        ("", "exactly one `export default"),
        (
            "import x from 'y';\nexport default <Page title=\"x\" />",
            "imports aren't allowed",
        ),
        (
            "export default function Page() { return <Page title=\"x\"/> }",
            "can't export code",
        ),
        (
            "export default () => <Page title=\"x\"/>",
            "functions aren't allowed",
        ),
        (
            "export default <Page title=\"x\"/>;\nexport const y = 1;",
            "only one default export",
        ),
        (
            "export default <Page title=\"x\"/>;\nconsole.log(1)",
            "unexpected text after the page",
        ),
        (
            "const x = 1; export default <Page title=\"x\"/>",
            "exactly one `export default",
        ),
        ("export default <Page title=\"x\">", "never closed"),
        (
            "export default <Page title=\"x\"></Section>",
            "doesn't match",
        ),
        (
            "export default <Section key=\"s\" />",
            "root element must be <Page",
        ),
        ("export default <Page />", "<Page> needs title"),
        (
            "export default <Page title=\"x\" refresh=\"2s\" />",
            "refresh` must be one of",
        ),
        (
            "export default <Page title=\"x\"><Page title=\"y\"/></Page>",
            "must be the root",
        ),
        (
            "export default <Page title=\"x\">hello</Page>",
            "text directly inside <Page>",
        ),
        (
            "export default <Page title=\"x\" /* unterminated",
            "unterminated /* comment",
        ),
        (
            "export default <Page title=\"x\" title=\"y\" />",
            "set twice",
        ),
    ];
    for (src, needle) in cases {
        rejects(&space_page(src), needle);
    }
}

#[test]
fn bounds_are_enforced() {
    let huge = page(&format!(
        "<Text key=\"t\">{}</Text>",
        "x".repeat(super::tsx::MAX_SOURCE_BYTES)
    ));
    rejects(&space_page(&huge), "at most 64 KB");
    let mut deep = String::new();
    for i in 0..14 {
        deep.push_str(&format!("<Section key=\"s{i}\">"));
    }
    deep.push_str("<Divider key=\"d\"/>");
    for _ in 0..14 {
        deep.push_str("</Section>");
    }
    rejects(&space_page(&page(&deep)), "nested more than 12 deep");
    let many: String = (0..401)
        .map(|i| format!("<Divider key=\"d{i}\"/>"))
        .collect();
    rejects(&space_page(&page(&many)), "at most 400 blocks");
    let data: String = (0..17)
        .map(|i| format!("<Data name=\"d{i}\" source=\"agents\"/>"))
        .collect();
    rejects(&space_page(&page(&data)), "at most 16 <Data>");
    let buttons: String = (0..33)
        .map(|i| format!("<Button key=\"b{i}\" label=\"x\" action=\"refresh\"/>"))
        .collect();
    rejects(&space_page(&page(&buttons)), "at most 32 buttons");
    let nots = format!(
        "<Data name=\"r\" source=\"runs\"/><Show key=\"s\" when={{{}r.active}}><Divider key=\"d\"/></Show>",
        "!".repeat(20)
    );
    rejects(&space_page(&page(&nots)), "nested too deeply");
    // Many diagnostics are capped, never unbounded.
    let bad: String = (0..100).map(|i| format!("<Nope{i} />")).collect();
    let c = space_page(&page(&bad));
    assert_eq!(c.diagnostics.len(), super::compile::MAX_DIAGNOSTICS);
}

#[test]
fn catalogue_rules() {
    let cases: &[(&str, &str)] = &[
        (r#"<Stat key="s" value={1} />"#, "<Stat> needs label"),
        (
            r#"<Stat key="s" label="x" value={1} tone="red" />"#,
            "tone` must be one of",
        ),
        (
            r#"<Stat key="s" label="x" value={1} colour="red" />"#,
            "unknown property `colour`",
        ),
        (r#"<Divider key="d">x</Divider>"#, "has no children"),
        (
            r#"<Row key="r">loose text</Row>"#,
            "text directly inside <Row>",
        ),
        (r#"<Grid key="g" columns={7} />"#, "columns must be 1..=4"),
        (
            r#"<Table key="t" rows={1}><Column field="a" label="A"/></Table>"#,
            "must read data",
        ),
        (
            r#"<Data name="r" source="runs"/><Table key="t" rows={r.items}><Text key="x">a</Text></Table>"#,
            "only <Column>",
        ),
        (
            r#"<Data name="r" source="runs"/><Table key="t" rows={r.items}></Table>"#,
            "at least one <Column",
        ),
        (
            r#"<Data name="r" source="runs"/><Table key="t" rows={r.items}><Column field="__proto__" label="A"/></Table>"#,
            "field must be a path",
        ),
        (
            r#"<Data name="r" source="runs"/><Stat key="s" label="x" value={r.itmes} />"#,
            "has no field `itmes`; it has: active, items",
        ),
        (
            r#"<Data name="r" source="runs"/><List key="l" items={r.items} as="run"><Text key="t">{run.nme}</Text></List>"#,
            "an item of `runs` has no field `nme`",
        ),
        (
            r#"<Data name="r" source="runs"/><Text key="t">{run.status}</Text>"#,
            "unknown name `run`",
        ),
        (
            r#"<Data name="r" source="runs"/><Data name="r" source="agents"/>"#,
            "declared twice",
        ),
        (
            r#"<Data name="r" source="weather"/>"#,
            "source` must be one of",
        ),
        (r#"<Data name="r" source="messages"/>"#, "needs room="),
        (
            r#"<Data name="r" source="runs" query="x"/>"#,
            "doesn't take `query`",
        ),
        (
            r#"<Data name="r" source="runs" limit={500}/>"#,
            "limit must be 1..=50",
        ),
        (
            r#"<Data name="r" source="runs" room="nowhere"/>"#,
            "no room named \"nowhere\"",
        ),
        (
            r#"<Data name="r" source="runs" agent="ghost"/>"#,
            "no agent @ghost",
        ),
        (
            r#"<Data name="r" source="runs" space="s-team"/>"#,
            "reads only its own Space",
        ),
        (
            r#"<Data name="r" source="agentAnswer" room="general" block="nope"/>"#,
            "must be the key of a <Button action=\"runAgent\">",
        ),
        (
            r#"<Data name="r" source="runs"/><AgentAnswer key="a" from={r} />"#,
            "must name a <Data source=\"agentAnswer\">",
        ),
        (
            r#"<Section key="a"><Data name="r" source="runs"/></Section>"#,
            "direct child of <Page>",
        ),
        (
            r#"<Column field="a" label="b"/>"#,
            "directly inside <Table>",
        ),
        (
            r#"<Stat key="a b" label="x" value={1}/>"#,
            "a key is 1..=64",
        ),
        (
            r#"<Stat key="a" label="x" value={1}/><Stat key="a" label="y" value={2}/>"#,
            "used twice",
        ),
        (r#"<Markdown key="m">{x}</Markdown>"#, "literal text only"),
        (
            r#"<Text key="t"><Badge key="b">x</Badge></Text>"#,
            "holds text, not blocks",
        ),
        // Actions.
        (
            r#"<Button key="b" label="x" action="runAgent" room="general" prompt="hi"/>"#,
            "needs agent=",
        ),
        (
            r#"<Button key="b" label="x" action="runAgent" agent="agent" prompt="hi"/>"#,
            "needs room=",
        ),
        (
            r#"<Button key="b" label="x" action="runAgent" agent="agent" room="general"/>"#,
            "needs prompt=",
        ),
        (
            r#"<Button key="b" label="x" action="runAgent" agent="agent" room="general" prompt={x}/>"#,
            "must be a literal string",
        ),
        (
            r#"<Button key="b" label="x" action="refresh" prompt="hi"/>"#,
            "doesn't take `prompt`",
        ),
        (
            r#"<Button key="b" label="x" action="runSchedule" schedule="nope"/>"#,
            "scheduled task id",
        ),
        (
            r#"<Button key="b" label="x" action="deleteEverything"/>"#,
            "action` must be one of",
        ),
        (
            r#"<Link key="l" label="x" action="runAgent" agent="agent" room="general" prompt="x"/>"#,
            "action` must be one of: openRoom, openPage",
        ),
        (
            r#"<Link key="l" label="x" action="openPage" page="https://evil.example"/>"#,
            "needs page=\"pg_",
        ),
        (
            r#"<Button key="b" label="x" action="runAgent" agent="agent" room="chat" prompt="x"/>"#,
            "no room named \"chat\"",
        ),
    ];
    for (body, needle) in cases {
        rejects(&space_page(&page(body)), needle);
    }
    let long = format!(
        r#"<Button key="b" label="x" action="runAgent" agent="agent" room="general" prompt="{}"/>"#,
        "x".repeat(2_001)
    );
    rejects(&space_page(&page(&long)), "prompt must be 1..=2000");
}

#[test]
fn personal_pages_name_spaces_and_may_use_their_chat() {
    let body = r#"
    <Data name="chat" source="messages" room="chat" />
    <Data name="team" source="messages" space="s-team" room="general" />
    <Data name="mine" source="library" />
    <Data name="ag" source="agents" space="s-team" />
    <Data name="all" source="runs" />
    <Button key="ask" label="Ask" action="runAgent" room="chat" prompt="Hello" />
    <Data name="answer" source="agentAnswer" room="chat" block="ask" />
    <Button key="ask2" label="Ask team" action="runAgent" space="s-team" room="general" agent="ci-bot" prompt="Hi" />"#;
    let c = personal_page(&page(body));
    ok(&c);
    let spec = c.spec.unwrap();
    assert!(spec.data[0].params.chat);
    assert_eq!(spec.data[1].params.space_id.as_deref(), Some("s-team"));
    assert_eq!(spec.data[1].params.room_id.as_deref(), Some("r-general"));
    let ask = spec.block("ask").unwrap().action.clone().unwrap();
    assert!(ask.target.chat);
    assert_eq!(ask.target.agent_id, None);
    let ask2 = spec.block("ask2").unwrap().action.clone().unwrap();
    assert_eq!(ask2.target.space_id.as_deref(), Some("s-team"));

    for (body, needle) in [
        (
            r#"<Data name="r" source="messages" room="general"/>"#,
            "name the Space too",
        ),
        (
            r#"<Data name="r" source="messages" space="s-other" room="general"/>"#,
            "no Space s-other",
        ),
        (r#"<Data name="r" source="agents"/>"#, "needs space="),
        (
            r#"<Button key="b" label="x" action="runAgent" room="chat" agent="agent" prompt="x"/>"#,
            "personal Chat has only its own agent",
        ),
    ] {
        rejects(&personal_page(&page(body)), needle);
    }
}

#[test]
fn diagnostics_point_at_the_right_place() {
    let src = "export default (\n  <Page title=\"x\">\n    <Stat key=\"s\" label=\"x\" value={go()} />\n  </Page>\n);";
    let c = space_page(src);
    let d = &c.diagnostics[0];
    assert_eq!((d.line, d.column), (3, 36), "{d:?}");
    let src = "export default (\n  <Page title=\"x\">\n    <Stat key=\"k1\" label=\"x\" value={1} tone=\"red\" />\n  </Page>\n);";
    let c = space_page(src);
    let d = &c.diagnostics[0];
    assert_eq!(d.key.as_deref(), Some("k1"));
    assert_eq!(d.line, 3);
    assert!(d.to_string().starts_with("line 3, column "), "{d}");
}

#[test]
fn the_spec_round_trips_through_json() {
    let c = space_page(RELEASE_WATCH);
    let spec = c.spec.unwrap();
    let json = serde_json::to_string(&spec).unwrap();
    let back: super::spec::PageSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(back, spec);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["format"], 1);
    assert_eq!(v["blocks"][0]["type"], "Row");
    assert_eq!(v["blocks"][0]["children"][0]["props"]["value"]["k"], "bind");
}

#[test]
fn arbitrary_input_never_panics() {
    // A small deterministic fuzz: mutations of a valid page.
    let base = RELEASE_WATCH.as_bytes();
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let alphabet = b"<>{}()=\"'`/!?.:;,&|+-*[]$\\ \nabcPageText0123";
    for _ in 0..3_000 {
        let mut bytes = base.to_vec();
        for _ in 0..(next() % 6 + 1) {
            let at = (next() as usize) % bytes.len();
            match next() % 3 {
                0 => bytes[at] = alphabet[(next() as usize) % alphabet.len()],
                1 => {
                    bytes.remove(at);
                }
                _ => bytes.insert(at, alphabet[(next() as usize) % alphabet.len()]),
            }
        }
        let src = String::from_utf8_lossy(&bytes);
        let c = space_page(&src);
        assert!(c.spec.is_some() != !c.diagnostics.is_empty());
        if let Some(spec) = &c.spec {
            invariants(spec);
        }
    }
    for weird in [
        "\u{0}",
        "<",
        "{",
        "export default (",
        "export default <Page title=\"\u{1F600}\"/>",
    ] {
        let _ = space_page(weird);
    }
}

#[test]
fn deep_and_long_expressions_are_refused_without_overflowing() {
    let when = |expr: String| {
        page(&format!(
            "<Data name=\"r\" source=\"runs\"/><Show key=\"s\" when={{{expr}}}><Divider key=\"d\"/></Show>"
        ))
    };
    // Review H1: a 15k-operand ?? chain used to recurse without bound.
    for n in [15_000, 21_000] {
        let chain = vec!["r"; n].join("??");
        let src = when(chain);
        assert!(src.len() < super::tsx::MAX_SOURCE_BYTES);
        rejects(&space_page(&src), "at most 8 values");
    }
    let eight = vec!["r.active"; 8].join(" ?? ");
    ok(&space_page(&when(eight)));
    let nine = vec!["r.active"; 9].join(" ?? ");
    rejects(&space_page(&when(nine)), "at most 8 values");
    // Nesting, negation and long mixtures are bounded too.
    let parens = format!("{}r{}", "(".repeat(15_000), ")".repeat(15_000));
    rejects(&space_page(&when(parens)), "nested too deeply");
    rejects(
        &space_page(&when(format!("{}r", "!".repeat(15_000)))),
        "nested too deeply",
    );
    let wide = vec!["(!!!!!r.active)"; 8].join(" ?? ");
    rejects(&space_page(&when(wide)), "too long");
    let nested_chains = format!(
        "({0}) ?? ({0}) ?? ({0}) ?? ({0})",
        vec!["(r.active ?? (r.items ?? (r.active ?? r.items)))"; 2].join(" ?? ")
    );
    let c = space_page(&when(nested_chains));
    if let Some(spec) = &c.spec {
        invariants(spec);
    } else {
        assert!(!c.diagnostics.is_empty());
    }
    // Thousands of nested elements stop at the depth limit.
    let deep = format!(
        "{}<Divider/>{}",
        "<Row>".repeat(5_000),
        "</Row>".repeat(5_000)
    );
    rejects(&space_page(&page(&deep)), "nested more than 12 deep");
}

#[test]
fn control_and_direction_characters_are_refused_however_written() {
    for bad in [
        '\u{7}', '\u{1b}', '\u{85}', '\u{202E}', '\u{202A}', '\u{2066}', '\u{2069}', '\u{200E}',
        '\u{200F}', '\r',
    ] {
        let raw = bad.to_string();
        let escaped = format!("\\u{:04x}", bad as u32);
        let entity = format!("&#x{:x};", bad as u32);
        let cases = [
            format!("<Text key=\"t\">a{raw}b</Text>"),
            format!("<Stat key=\"s\" label=\"a{raw}b\" value={{1}}/>"),
            format!("<Stat key=\"s\" label=\"a{entity}b\" value={{1}}/>"),
            format!("<Text key=\"t\">a{entity}b</Text>"),
            format!("<Stat key=\"s\" label={{\"a{escaped}b\"}} value={{1}}/>"),
            format!("<Stat key=\"s\" label={{\"a{raw}b\"}} value={{1}}/>"),
            format!("<Text key=\"t\">{{`a{raw}b`}}</Text>"),
            format!(
                "<Button key=\"b\" label=\"x\" action=\"runAgent\" agent=\"agent\" room=\"general\" prompt=\"run {raw}rm\"/>"
            ),
        ];
        for body in cases {
            let c = space_page(&page(&body));
            // Numeric entities for C0/C1 controls stay literal text (they
            // aren't decoded), which is harmless; everything else is refused.
            if body.contains("&#x") && bad.is_control() {
                if let Some(spec) = &c.spec {
                    assert!(!serde_json::to_string(spec).unwrap().contains(bad));
                    continue;
                }
            }
            assert!(
                c.spec.is_none(),
                "accepted U+{:04X} in {body:?}",
                bad as u32
            );
            assert!(
                c.diagnostics
                    .iter()
                    .any(|d| d.message.contains("isn't allowed in a page")),
                "{body:?}: {:#?}",
                c.diagnostics
            );
        }
    }
    // Line breaks and tabs are fine.
    ok(&space_page(&page(
        "<Markdown key=\"m\">{\"a\\n\\tb\"}</Markdown>",
    )));
}

#[test]
fn markdown_never_loads_images_or_opens_other_schemes() {
    for (md, needle) in [
        ("![x](https://evil.example/p.png)", "can't show images"),
        ("see [here](http://evil.example)", "https://"),
        ("[x](javascript:alert(1))", "https://"),
        ("[x]( JAVASCRIPT:alert(1))", "https://"),
        ("[x](data:text/html,hi)", "https://"),
        ("[x](file:///etc/passwd)", "https://"),
        ("[x][r]\n\n[r]: http://evil.example", "https://"),
        ("go to http://evil.example", "https://"),
        ("go to www.evil.example", "https://"),
        ("<img src=x>", "raw HTML"),
        ("<https://evil.example>", "raw HTML"),
        ("<!-- x -->", "raw HTML"),
    ] {
        let body = format!(
            "<Markdown key=\"m\">{{{}}}</Markdown>",
            serde_json::to_string(md).unwrap()
        );
        rejects(&space_page(&page(&body)), needle);
    }
    ok(&space_page(&page(
        "<Markdown key=\"m\">See [the docs](https://example.com/docs) and a &lt; b.</Markdown>",
    )));
}

#[test]
fn data_declarations_are_never_silently_dropped() {
    // A string in braces is the same string.
    let c = space_page(&page(
        "<Data name=\"m\" source=\"messages\" room={\"general\"} agent={\"agent\"}/><MessageFeed key=\"f\" from={m}/>",
    ));
    ok(&c);
    let spec = c.spec.unwrap();
    assert_eq!(spec.data[0].params.room_id.as_deref(), Some("r-general"));
    assert_eq!(spec.data[0].params.agent_id.as_deref(), Some("ag-1"));
    // Anything else fails the whole page with a diagnostic.
    for body in [
        "<Data name=\"m\" source=\"messages\" room={1}/><MessageFeed key=\"f\" from={m}/>",
        "<Data name=\"m\" source=\"messages\" room={true}/><MessageFeed key=\"f\" from={m}/>",
        "<Data name=\"m\" source=\"messages\" room={null}/><MessageFeed key=\"f\" from={m}/>",
        "<Data name={\"m\"} source={1} room=\"general\"/><MessageFeed key=\"f\" from={m}/>",
        "<Data name=\"m\" source=\"runs\" space={\"s-team\"}/><RunList key=\"f\" from={m}/>",
        "<Data name=\"m\" source=\"messages\"/><MessageFeed key=\"f\" from={m}/>",
    ] {
        let c = space_page(&page(body));
        assert!(c.spec.is_none(), "accepted {body}");
        assert!(!c.diagnostics.is_empty());
    }
}

#[test]
fn reserved_names_and_chart_fields_are_checked() {
    for (body, needle) in [
        (
            "<Stat key=\"constructor\" label=\"x\" value={1}/>",
            "reserved name",
        ),
        (
            "<Stat key=\"__proto__\" label=\"x\" value={1}/>",
            "reserved name",
        ),
        (
            "<Stat key=\"toString\" label=\"x\" value={1}/>",
            "reserved name",
        ),
        (
            "<Data name=\"constructor\" source=\"runs\"/>",
            "reserved name",
        ),
        ("<Data name=\"valueOf\" source=\"runs\"/>", "reserved name"),
        (
            "<Data name=\"r\" source=\"runs\"/><Each key=\"e\" items={r.items} as=\"hasOwnProperty\"><Divider key=\"d\"/></Each>",
            "reserved name",
        ),
        (
            "<Data name=\"r\" source=\"runs\"/><Chart key=\"c\" kind=\"bar\" data={r.items} x=\"__proto__\" y=\"status\"/>",
            "field must be a path",
        ),
        (
            "<Data name=\"r\" source=\"runs\"/><Chart key=\"c\" kind=\"bar\" data={r.items} x=\"agent\" y=\"a.constructor\"/>",
            "field must be a path",
        ),
        (
            "<Data name=\"r\" source=\"runs\"/><Chart key=\"c\" kind=\"bar\" data={r.items} x=\"a b\" y=\"status\"/>",
            "field must be a path",
        ),
        (
            "<Data name=\"r\" source=\"runs\"/><Table key=\"t\" rows={r.items}><Column field=\"toString\" label=\"A\"/></Table>",
            "field must be a path",
        ),
        (
            "<Data name=\"r\" source=\"runs\"/><Stat key=\"s\" label=\"x\" value={r.items.valueOf}/>",
            "can't be read",
        ),
    ] {
        rejects(&space_page(&page(body)), needle);
    }
    ok(&space_page(&page(
        "<Data name=\"r\" source=\"runs\"/><Chart key=\"c\" kind=\"bar\" data={r.items} x=\"agent\" y=\"status\"/>",
    )));
}

mod store_tests {
    use super::super::limits::PageLimits;
    use super::super::store::{self, CreateError, NewVersion};
    use crate::db::Db;

    fn version(author: &str, title: &str) -> NewVersion {
        NewVersion {
            author_id: author.to_owned(),
            kind: "manual",
            prompt: None,
            model_ref: None,
            restored_from: None,
            source: format!("export default <Page title=\"{title}\" />"),
            spec: "{}".to_owned(),
            title: title.to_owned(),
        }
    }

    fn db_with_space() -> (Db, String) {
        let db = Db::open_in_memory().unwrap();
        let space = db.create_space("alice", "Team", 10).unwrap();
        db.add_member(&space.id, "bob", 10).unwrap();
        (db, space.id)
    }

    #[test]
    fn page_limits_count_live_pages_per_space_and_person() {
        let (db, sid) = db_with_space();
        let limits = PageLimits {
            pages_per_space: 2,
            versions_per_page: 200,
        };
        let a = store::create(&db, Some(&sid), &version("alice", "A"), &limits)
            .unwrap()
            .unwrap();
        store::create(&db, Some(&sid), &version("bob", "B"), &limits)
            .unwrap()
            .unwrap();
        assert_eq!(
            store::create(&db, Some(&sid), &version("alice", "C"), &limits).unwrap(),
            Err(CreateError::TooMany(2))
        );
        // Personal pages count per person, separately.
        store::create(&db, None, &version("alice", "Mine"), &limits)
            .unwrap()
            .unwrap();
        // Deleting frees a slot; undeleting into a full Space is refused.
        assert!(store::soft_delete(&db, &a.id, "alice").unwrap());
        store::create(&db, Some(&sid), &version("alice", "C"), &limits)
            .unwrap()
            .unwrap();
        assert_eq!(
            store::undelete(&db, &a.id, &limits).unwrap(),
            Err(CreateError::TooMany(2))
        );
    }

    #[test]
    fn versions_are_optimistic_and_pruned_keeping_the_first() {
        let (db, sid) = db_with_space();
        let limits = PageLimits {
            pages_per_space: 50,
            versions_per_page: 3,
        };
        let p = store::create(&db, Some(&sid), &version("alice", "v1"), &limits)
            .unwrap()
            .unwrap();
        for n in 1..6 {
            let head = store::add_version(
                &db,
                &p.id,
                n,
                &version("alice", &format!("v{}", n + 1)),
                &limits,
            )
            .unwrap()
            .unwrap();
            assert_eq!(head, n + 1);
        }
        // A save based on an old head never overwrites.
        assert_eq!(
            store::add_version(&db, &p.id, 3, &version("alice", "stale"), &limits).unwrap(),
            Err(store::Conflict { head: 6 })
        );
        let kept: Vec<i64> = store::versions(&db, &p.id)
            .unwrap()
            .iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(kept, vec![6, 5, 4, 1]);
        let page = store::get(&db, &p.id).unwrap().unwrap();
        assert_eq!(page.title, "v6");
    }

    #[test]
    fn leaving_and_deleting_a_space_purge_page_state() {
        let (db, sid) = db_with_space();
        let limits = PageLimits::default();
        let p = store::create(&db, Some(&sid), &version("bob", "Bob's"), &limits)
            .unwrap()
            .unwrap();
        store::put_prefs(
            &db,
            "bob",
            &p.id,
            &store::Prefs {
                pinned: true,
                position: Some(1),
            },
        )
        .unwrap();
        store::confirm(&db, "bob", &p.id, 1).unwrap();
        store::create_draft(
            &db,
            &store::NewDraft {
                page_id: Some(&p.id),
                space_id: Some(&sid),
                author_id: "bob",
                base_version: Some(1),
                block_key: None,
                prompt: "x",
            },
        )
        .unwrap();
        db.remove_member(&sid, "bob").unwrap();
        assert!(!store::prefs(&db, "bob", &p.id).unwrap().pinned);
        assert!(!store::confirmed(&db, "bob", &p.id, 1).unwrap());
        let drafts: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM page_drafts",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(drafts, 0);
        // The page Bob created stays, like his messages.
        assert!(store::get(&db, &p.id).unwrap().is_some());
        // Deleting the Space removes its pages and versions.
        let personal = store::create(&db, None, &version("alice", "Mine"), &limits)
            .unwrap()
            .unwrap();
        assert!(matches!(
            db.delete_space(&sid, "alice", "Team").unwrap(),
            crate::spaces::Deleted::Done { .. }
        ));
        assert!(store::get(&db, &p.id).unwrap().is_none());
        let versions: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM page_versions WHERE page_id = ?1",
                [&p.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(versions, 0);
        assert!(store::get(&db, &personal.id).unwrap().is_some());
    }

    #[test]
    fn soft_deleted_pages_are_purged_after_retention() {
        let (db, sid) = db_with_space();
        let p = store::create(
            &db,
            Some(&sid),
            &version("alice", "A"),
            &PageLimits::default(),
        )
        .unwrap()
        .unwrap();
        store::soft_delete(&db, &p.id, "alice").unwrap();
        let now = crate::db::now_ms();
        assert_eq!(store::purge_expired(&db, now).unwrap(), 0);
        assert_eq!(
            store::purge_expired(&db, now + store::DELETED_RETENTION_MS + 1).unwrap(),
            1
        );
        assert!(store::get(&db, &p.id).unwrap().is_none());
    }
}

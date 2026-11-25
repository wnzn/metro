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
    fn room(&mut self, space: &str, name: &str) -> Result<(String, String, bool), String> {
        assert_eq!(space, "s-team");
        match name.to_ascii_lowercase().as_str() {
            "general" => Ok(("r-general".into(), "general".into(), false)),
            "releases" => Ok(("r-releases".into(), "releases".into(), false)),
            "deploy-thread" => Ok(("r-thread".into(), "deploy-thread".into(), true)),
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
    <Data name="answer" source="agentAnswer" room="chat" block="ask" />"#;
    let c = personal_page(&page(body));
    ok(&c);
    let spec = c.spec.unwrap();
    assert!(spec.data[0].params.chat);
    assert_eq!(spec.data[1].params.space_id.as_deref(), Some("s-team"));
    assert_eq!(spec.data[1].params.room_id.as_deref(), Some("r-general"));
    let ask = spec.block("ask").unwrap().action.clone().unwrap();
    assert!(ask.target.chat);
    assert_eq!(ask.target.agent_id, None);

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
fn personal_page_buttons_are_strictly_page_only() {
    for (body, needle) in [
        (
            r#"<Button key="b" label="x" action="runAgent" space="s-team" room="general" agent="ci-bot" prompt="Hi"/>"#,
            "only start private runs shown on the page",
        ),
        (
            r#"<Button key="b" label="x" action="runSchedule" schedule="sch_0123456789abcdef0123456789abcdef"/>"#,
            "a personal page can't run schedules",
        ),
    ] {
        rejects(&personal_page(&page(body)), needle);
    }
    // The page-only run, and navigation, stay.
    ok(&personal_page(&page(
        r#"<Button key="b" label="x" action="runAgent" room="chat" prompt="Hi"/>
           <Link key="l" label="Open" action="openRoom" space="s-team" room="general"/>"#,
    )));
}

#[test]
fn buttons_never_send_work_to_threads() {
    rejects(
        &space_page(&page(
            r#"<Button key="b" label="x" action="runAgent" agent="agent" room="deploy-thread" prompt="Hi"/>"#,
        )),
        "can't send work to a thread",
    );
    // Reading a thread is fine.
    ok(&space_page(&page(
        r#"<Data name="m" source="messages" room="deploy-thread"/><MessageFeed key="f" from={m}/>"#,
    )));
}

#[test]
fn repetition_is_bounded() {
    for body in [
        r#"<Data name="r" source="runs"/><Each key="a" items={r.items} as="x"><Each key="b" items={r.items} as="y"><Divider key="d"/></Each></Each>"#,
        r#"<Data name="r" source="runs"/><List key="a" items={r.items} as="x"><Section key="s"><List key="b" items={r.items} as="y"><Divider key="d"/></List></Section></List>"#,
        r#"<Data name="r" source="runs"/><Each key="a" items={r.items} as="x"><Table key="t" rows={r.items}><Column field="id" label="Id"/></Table></Each>"#,
    ] {
        rejects(&space_page(&page(body)), "can't be nested");
    }
    // 101 blocks repeated 50 times is over the render budget.
    let many: String = (0..101)
        .map(|i| format!("<Divider key=\"d{i}\"/>"))
        .collect();
    let body = format!(
        r#"<Data name="r" source="runs"/><Each key="e" items={{r.items}} as="x">{many}</Each>"#
    );
    rejects(&space_page(&page(&body)), "would draw more than");
    let cols: String = (0..13)
        .map(|i| format!("<Column field=\"c{i}\" label=\"C\"/>"))
        .collect();
    let body =
        format!(r#"<Data name="r" source="runs"/><Table key="t" rows={{r.items}}>{cols}</Table>"#);
    rejects(&space_page(&page(&body)), "at most 12 columns");
    ok(&space_page(&page(
        r#"<Data name="r" source="runs"/><Each key="e" items={r.items} as="x"><Text key="t">{x.status}</Text></Each>"#,
    )));
}

#[test]
fn attributes_entities_and_the_keyed_source_are_bounded() {
    let attrs: String = (0..25).map(|i| format!(" a{i}=\"x\"")).collect();
    rejects(
        &space_page(&page(&format!("<Divider key=\"d\"{attrs}/>"))),
        "at most 24 properties",
    );
    // Many '&' without ';' are scanned in linear time.
    let amps = "&".repeat(60_000);
    let started = std::time::Instant::now();
    let _ = space_page(&page(&format!("<Text key=\"t\">{amps}</Text>")));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    // A source just under the limit whose generated keys push it over.
    let texts: String = (0..4)
        .map(|i| format!("<Text key=\"t{i}\">{}</Text>", "x".repeat(15_500)))
        .collect();
    let dividers = "<Divider/>".repeat(150);
    let src = format!("export default <Page title=\"x\">{dividers}{texts}</Page>");
    assert!(src.len() <= super::tsx::MAX_SOURCE_BYTES, "{}", src.len());
    rejects(&space_page(&src), "with its keys added the page is over");
}

#[test]
fn markdown_links_are_checked_structurally() {
    for (md, needle) in [
        (
            "[x](https://a.example) and [y](  http://b.example )",
            "https://",
        ),
        ("[x](<javascript:alert(1)>)", "https://"),
        ("[x](https://a.example\\)) [y](javascript:x)", "https://"),
        ("[x](https://a.example \"t\") [y](ftp://b)", "https://"),
        ("[x][ref]", "reference-style"),
        ("[x][]", "reference-style"),
        ("   [ref]: https://a.example", "reference-style"),
        ("[x](https://a.example&#58;)", "https://"),
        ("[x](https://a.example", "https://"),
    ] {
        let body = format!(
            "<Markdown key=\"m\">{{{}}}</Markdown>",
            serde_json::to_string(md).unwrap()
        );
        rejects(&space_page(&page(&body)), needle);
    }
    for md in [
        "[docs](https://example.com/a_(b)) and [more](<https://example.com/c>)",
        "[t](https://example.com \"Title\")",
    ] {
        let body = format!(
            "<Markdown key=\"m\">{{{}}}</Markdown>",
            serde_json::to_string(md).unwrap()
        );
        ok(&space_page(&page(&body)));
    }
}

#[test]
fn template_placeholders_are_replaced_in_one_pass() {
    let t = super::generate::template("release-watch").unwrap();
    let src = super::generate::instantiate(t, Some(("r-1", "__ROOM_ONLY__ __TARGET__", "agent")));
    // The room name is inserted verbatim; nothing inside it is expanded.
    assert_eq!(src.matches("__ROOM_ONLY__ __TARGET__").count(), 3, "{src}");
    assert!(!src.contains("room=room="), "{src}");
}

#[test]
fn moves_and_sibling_swaps_count_outside_the_selection() {
    let a = space_page(&page(
        r#"<Section key="s"><Text key="a">A</Text><Text key="b">B</Text></Section><Section key="sel"><Text key="in">I</Text></Section>"#,
    ))
    .spec
    .unwrap();
    // A sibling swap outside the selected block.
    let swapped = space_page(&page(
        r#"<Section key="s"><Text key="b">B</Text><Text key="a">A</Text></Section><Section key="sel"><Text key="in">I</Text></Section>"#,
    ))
    .spec
    .unwrap();
    let c = super::generate::changes(Some(&a), &swapped);
    assert_eq!(
        super::generate::outside_selection(&a, &swapped, "sel", &c),
        vec!["s".to_owned()]
    );
    // A block moved from elsewhere into the selection.
    let moved = space_page(&page(
        r#"<Section key="s"><Text key="b">B</Text></Section><Section key="sel"><Text key="in">I</Text><Text key="a">A</Text></Section>"#,
    ))
    .spec
    .unwrap();
    let c = super::generate::changes(Some(&a), &moved);
    let out = super::generate::outside_selection(&a, &moved, "sel", &c);
    assert!(out.contains(&"s".to_owned()), "{out:?}");
    // Top-level order counts too.
    let reordered = space_page(&page(
        r#"<Section key="sel"><Text key="in">I</Text></Section><Section key="s"><Text key="a">A</Text><Text key="b">B</Text></Section>"#,
    ))
    .spec
    .unwrap();
    let c = super::generate::changes(Some(&a), &reordered);
    assert_eq!(
        super::generate::outside_selection(&a, &reordered, "sel", &c),
        vec!["page".to_owned()]
    );
    // A change inside the selection is inside.
    let inside = space_page(&page(
        r#"<Section key="s"><Text key="a">A</Text><Text key="b">B</Text></Section><Section key="sel"><Text key="in">changed</Text><Text key="new">N</Text></Section>"#,
    ))
    .spec
    .unwrap();
    let c = super::generate::changes(Some(&a), &inside);
    assert!(super::generate::outside_selection(&a, &inside, "sel", &c).is_empty());
}

#[test]
fn a_huge_invalid_subtree_is_costed_without_overflow() {
    // Repeaters nested as deep as the parser allows around a table with
    // hundreds of columns: worst-case cost far past usize. Rejected with
    // diagnostics, and the costing never overflows (debug build).
    let mut body = String::from(r#"<Data name="r" source="runs"/>"#);
    for i in 0..9 {
        body.push_str(&format!(r#"<Each key="e{i}" items={{r.items}} as="x{i}">"#));
    }
    let cols: String = (0..500)
        .map(|c| format!(r#"<Column field="c{c}" label="C"/>"#))
        .collect();
    body.push_str(&format!(
        r#"<Table key="t" rows={{r.items}}>{cols}</Table>"#
    ));
    body.push_str(&"</Each>".repeat(9));
    let c = space_page(&page(&body));
    assert!(c.spec.is_none());
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.message.contains("can't be nested")),
        "{:#?}",
        c.diagnostics
    );
}

#[test]
fn reference_definitions_are_refused_in_any_container() {
    for md in [
        "> [x]: https://a.example\n>\n> [x]",
        "- [x]: https://a.example\n- [x]",
        "1. [x]: https://a.example",
        "[multi\nline]: https://a.example",
        "para\n[x]:\n  https://a.example",
        "* > [x]: javascript:alert(1)",
    ] {
        let body = format!(
            "<Markdown key=\"m\">{{{}}}</Markdown>",
            serde_json::to_string(md).unwrap()
        );
        rejects(&space_page(&page(&body)), "reference-style");
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
    let eight = ["r.active"; 8].join(" ?? ");
    ok(&space_page(&when(eight)));
    let nine = ["r.active"; 9].join(" ?? ");
    rejects(&space_page(&when(nine)), "at most 8 values");
    // Nesting, negation and long mixtures are bounded too.
    let parens = format!("{}r{}", "(".repeat(15_000), ")".repeat(15_000));
    rejects(&space_page(&when(parens)), "nested too deeply");
    rejects(
        &space_page(&when(format!("{}r", "!".repeat(15_000)))),
        "nested too deeply",
    );
    let wide = ["(!!!!!r.active)"; 8].join(" ?? ");
    rejects(&space_page(&when(wide)), "too long");
    let nested_chains = format!(
        "({0}) ?? ({0}) ?? ({0}) ?? ({0})",
        ["(r.active ?? (r.items ?? (r.active ?? r.items)))"; 2].join(" ?? ")
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
            if body.contains("&#x")
                && bad.is_control()
                && let Some(spec) = &c.spec
            {
                assert!(!serde_json::to_string(spec).unwrap().contains(bad));
                continue;
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

#[test]
fn invisible_characters_and_comments_are_checked() {
    for bad in [
        '\u{061C}', '\u{200B}', '\u{200C}', '\u{2060}', '\u{2063}', '\u{FEFF}', '\u{00AD}',
        '\u{180E}',
    ] {
        let body = format!("<Text key=\"t\">a{bad}b</Text>");
        rejects(&space_page(&page(&body)), "isn't allowed in a page");
    }
    // Comments are stored and shown, so they follow the same rules.
    for comment in [
        "// hidden \u{202E}evil\n",
        "/* hidden \u{202E}evil */",
        "/* a\u{7}b */",
    ] {
        let src = format!("{comment}export default <Page title=\"x\"><Divider/></Page>");
        rejects(&space_page(&src), "isn't allowed in a page");
        let body = format!("{{{comment}}}");
        let body = body.replace("//", "/*").replace('\n', " */");
        rejects(
            &space_page(&page(&format!("<Section key=\"s\">{body}</Section>"))),
            "isn't allowed",
        );
    }
    // CRLF sources and ordinary comments are fine.
    ok(&space_page(
        "// a comment\r\nexport default <Page title=\"x\">{/* note\r\n more */}<Divider/></Page>",
    ));
    // The zero-width joiner only inside emoji sequences.
    for good in ["👩\u{200D}💻", "❤\u{FE0F}\u{200D}🔥", "👋🏽"] {
        ok(&space_page(&page(&format!(
            "<Badge key=\"b\">{good} ok</Badge>"
        ))));
        assert_eq!(super::tsx::invalid_char_in(good), None);
    }
    for bad in ["a\u{200D}b", "\u{200D}👩", "👩\u{200D}", "👩\u{200D}a"] {
        assert_eq!(
            super::tsx::invalid_char_in(bad),
            Some('\u{200D}'),
            "{bad:?}"
        );
        rejects(
            &space_page(&page(&format!("<Badge key=\"b\">{bad}</Badge>"))),
            "isn't allowed in a page",
        );
    }
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
        assert_eq!(store::soft_delete(&db, &a.id, "alice").unwrap(), Ok(()));
        store::create(&db, Some(&sid), &version("alice", "C"), &limits)
            .unwrap()
            .unwrap();
        assert_eq!(
            store::undelete(&db, &a.id, "alice", &limits).unwrap(),
            Err(store::UndeleteError::TooMany(2))
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
            Err(store::SaveError::Conflict { head: 6 })
        );
        let kept: Vec<i64> = store::version_list(&db, &p.id)
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
        .unwrap()
        .unwrap();
        db.lock()
            .execute(
                "INSERT INTO page_confirmations (user_id, page_id, version, created_at)
                 VALUES ('bob', ?1, 1, 0)",
                [&p.id],
            )
            .unwrap();
        store::create_draft(
            &db,
            &store::NewDraft {
                page_id: Some(&p.id),
                space_id: Some(&sid),
                author_id: "bob",
                block_key: None,
                prompt: "x",
                busy_window_ms: 60_000,
            },
        )
        .unwrap()
        .unwrap();
        db.remove_member(&sid, "bob").unwrap();
        assert!(!store::prefs(&db, "bob", &p.id).unwrap().pinned);
        assert!(!store::confirmed(&db, "bob", &p.id, 1).unwrap());
        let drafts: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM page_drafts WHERE status <> 'discarded'",
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
    fn saves_recheck_membership_and_edit_rights_in_the_transaction() {
        let (db, sid) = db_with_space();
        let limits = PageLimits::default();
        // Carol isn't a member: no Space page for her.
        assert_eq!(
            store::create(&db, Some(&sid), &version("carol", "x"), &limits).unwrap(),
            Err(CreateError::NotAllowed)
        );
        let p = store::create(&db, Some(&sid), &version("bob", "Bob's"), &limits)
            .unwrap()
            .unwrap();
        // Alice (owner) may; Dave-like members without rights may not.
        db.add_member(&sid, "dave", 10).unwrap();
        // Dave sees the page but may not edit it: 403, not 404.
        assert_eq!(
            store::add_version(&db, &p.id, 1, &version("dave", "x"), &limits).unwrap(),
            Err(store::SaveError::Denied(store::Denied::Forbidden))
        );
        let ops = store::PatchOps {
            members_edit: Some(true),
            ..Default::default()
        };
        assert_eq!(
            store::patch(&db, &p.id, "alice", &ops, &limits).unwrap(),
            Ok(())
        );
        assert_eq!(
            store::add_version(&db, &p.id, 1, &version("dave", "x"), &limits).unwrap(),
            Ok(2)
        );
        // Removed between the API's check and the write: refused.
        db.remove_member(&sid, "bob").unwrap();
        assert_eq!(
            store::add_version(&db, &p.id, 2, &version("bob", "y"), &limits).unwrap(),
            Err(store::SaveError::Denied(store::Denied::Gone))
        );
        assert_eq!(
            store::create(&db, Some(&sid), &version("bob", "z"), &limits).unwrap(),
            Err(CreateError::NotAllowed)
        );
        // Personal pages: only their owner.
        let mine = store::create(&db, None, &version("alice", "Mine"), &limits)
            .unwrap()
            .unwrap();
        assert_eq!(
            store::add_version(&db, &mine.id, 1, &version("bob", "x"), &limits).unwrap(),
            Err(store::SaveError::Denied(store::Denied::Gone))
        );
        assert_eq!(
            store::add_version(&db, &p.id, 2, &version("alice", "owner"), &limits).unwrap(),
            Ok(3)
        );
    }

    fn draft<'a>(author: &'a str, window: i64) -> store::NewDraft<'a> {
        store::NewDraft {
            page_id: None,
            space_id: None,
            author_id: author,
            block_key: None,
            prompt: "x",
            busy_window_ms: window,
        }
    }

    const WINDOW: i64 = 60_000;

    #[test]
    fn a_restart_frees_generation_slots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pages.sqlite");
        let (kept, discarded) = {
            let db = Db::open(&path).unwrap();
            let a = store::create_draft(&db, &draft("alice", WINDOW))
                .unwrap()
                .unwrap();
            let b = store::create_draft(&db, &draft("alice", WINDOW))
                .unwrap()
                .unwrap();
            assert!(store::discard_draft(&db, &b.id, "alice").unwrap());
            // Both still hold a slot: the third is refused.
            assert!(
                store::create_draft(&db, &draft("alice", WINDOW))
                    .unwrap()
                    .is_err()
            );
            (a.id, b.id)
        };
        // The server stopped with both jobs cut off.
        let db = Db::open(&path).unwrap();
        let a = store::draft(&db, &kept).unwrap().unwrap();
        assert_eq!(a.status, "failed");
        assert_eq!(a.error.as_deref(), Some(store::INTERRUPTED));
        let gone: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM page_drafts WHERE id = ?1",
                [&discarded],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gone, 0);
        for _ in 0..2 {
            assert!(
                store::create_draft(&db, &draft("alice", WINDOW))
                    .unwrap()
                    .is_ok()
            );
        }
    }

    #[test]
    fn a_draft_older_than_any_job_holds_no_slot() {
        let db = Db::open_in_memory().unwrap();
        for _ in 0..2 {
            store::create_draft(&db, &draft("alice", WINDOW))
                .unwrap()
                .unwrap();
        }
        assert!(
            store::create_draft(&db, &draft("alice", WINDOW))
                .unwrap()
                .is_err()
        );
        db.lock()
            .execute(
                "UPDATE page_drafts SET created_at = created_at - ?1",
                [WINDOW + 1],
            )
            .unwrap();
        assert!(
            store::create_draft(&db, &draft("alice", WINDOW))
                .unwrap()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_panicking_job_still_releases_its_slot() {
        let db = Db::open_in_memory().unwrap();
        let d = store::create_draft(&db, &draft("alice", WINDOW))
            .unwrap()
            .unwrap();
        store::create_draft(&db, &draft("alice", WINDOW))
            .unwrap()
            .unwrap();
        assert!(
            store::create_draft(&db, &draft("alice", WINDOW))
                .unwrap()
                .is_err()
        );
        let job = super::super::generate::guarded(db.clone(), d.id.clone(), async {
            panic!("the page builder blew up");
        });
        assert!(tokio::spawn(job).await.is_err(), "the job panicked");
        let after = store::draft(&db, &d.id).unwrap().unwrap();
        assert_eq!(after.status, "failed");
        assert_eq!(
            after.error.as_deref(),
            Some(super::super::generate::STOPPED)
        );
        assert!(
            store::create_draft(&db, &draft("alice", WINDOW))
                .unwrap()
                .is_ok()
        );
        // A job that finishes normally keeps its own result.
        let d = store::create_draft(&db, &draft("bob", WINDOW))
            .unwrap()
            .unwrap();
        let (db2, id) = (db.clone(), d.id.clone());
        super::super::generate::guarded(db.clone(), d.id.clone(), async move {
            store::finish_draft(
                &db2,
                &id,
                "ready",
                Some("s"),
                Some("{}"),
                None,
                None,
                None,
                1,
            )
            .unwrap();
        })
        .await;
        assert_eq!(store::draft(&db, &d.id).unwrap().unwrap().status, "ready");
    }

    #[test]
    fn protected_reads_join_live_membership() {
        let (db, sid) = db_with_space();
        db.append_event(
            &sid,
            "lobby",
            "message",
            &crate::db::Author::server("x"),
            "secret",
            &crate::db::Addressed::room(),
        )
        .unwrap();
        let read = |who: &str| {
            store::room_events(&db, who, &sid, "lobby", &["message"], None, None, 10)
                .unwrap()
                .len()
        };
        assert_eq!(read("bob"), 1);
        // Removed between an earlier check and this read: nothing.
        db.remove_member(&sid, "bob").unwrap();
        assert_eq!(read("bob"), 0);
        // The compiler's room and agent lookups say nothing to outsiders.
        use super::super::compile::Resolver;
        let mut r = super::super::access::DbResolver {
            db: &db,
            user: "bob",
            space: Some(sid.clone()),
        };
        let e = r.room(&sid, "nope").unwrap_err();
        assert!(!e.contains("lobby"), "{e}");
        let e = r.agent(&sid, "nope").unwrap_err();
        assert!(!e.contains("@agent"), "{e}");
    }

    #[test]
    fn expiry_and_the_restore_window_hold_on_direct_access() {
        let (db, sid) = db_with_space();
        let limits = PageLimits::default();
        let p = store::create(&db, Some(&sid), &version("alice", "A"), &limits)
            .unwrap()
            .unwrap();
        store::soft_delete(&db, &p.id, "alice").unwrap().unwrap();
        db.lock()
            .execute(
                "UPDATE pages SET deleted_at = deleted_at - ?1 - 1 WHERE id = ?2",
                rusqlite::params![store::DELETED_RETENTION_MS, p.id],
            )
            .unwrap();
        assert_eq!(
            store::undelete(&db, &p.id, "alice", &limits).unwrap(),
            Err(store::UndeleteError::Denied(store::Denied::Gone))
        );
        assert!(store::list_for(&db, "alice", true).unwrap().is_empty());
        // An expired ready draft can't be read or applied.
        let d = store::create_draft(&db, &draft("alice", WINDOW))
            .unwrap()
            .unwrap();
        store::finish_draft(
            &db,
            &d.id,
            "ready",
            Some("s"),
            Some("{}"),
            None,
            None,
            None,
            1,
        )
        .unwrap();
        db.lock()
            .execute(
                "UPDATE page_drafts SET expires_at = 0 WHERE id = ?1",
                [&d.id],
            )
            .unwrap();
        assert!(store::draft(&db, &d.id).unwrap().is_none());
        assert_eq!(
            store::apply_draft(&db, &d.id, "alice", &version("alice", "x"), &limits).unwrap(),
            Err(store::ApplyError::Gone)
        );
    }

    #[test]
    fn applying_a_draft_is_one_transaction() {
        let (db, sid) = db_with_space();
        let limits = PageLimits::default();
        let p = store::create(&db, Some(&sid), &version("bob", "B"), &limits)
            .unwrap()
            .unwrap();
        let new_draft = |db: &Db| {
            let d = store::create_draft(
                db,
                &store::NewDraft {
                    page_id: Some(&p.id),
                    space_id: Some(&sid),
                    author_id: "bob",
                    block_key: None,
                    prompt: "x",
                    busy_window_ms: WINDOW,
                },
            )
            .unwrap()
            .unwrap();
            store::finish_draft(
                db,
                &d.id,
                "ready",
                Some("s"),
                Some("{}"),
                None,
                None,
                None,
                1,
            )
            .unwrap();
            d.id
        };
        // A conflict keeps the draft.
        let d = new_draft(&db);
        store::add_version(&db, &p.id, 1, &version("bob", "v2"), &limits)
            .unwrap()
            .unwrap();
        assert_eq!(
            store::apply_draft(&db, &d, "bob", &version("bob", "x"), &limits).unwrap(),
            Err(store::ApplyError::Conflict { head: 2 })
        );
        assert!(store::draft(&db, &d).unwrap().is_some());
        // Lost rights keep it too, and nothing is written: Dave drafts while
        // members may edit, then the creator turns that off.
        db.add_member(&sid, "dave", 10).unwrap();
        let on = |v: bool| store::PatchOps {
            members_edit: Some(v),
            ..Default::default()
        };
        store::patch(&db, &p.id, "bob", &on(true), &limits)
            .unwrap()
            .unwrap();
        let d2 = store::create_draft(
            &db,
            &store::NewDraft {
                page_id: Some(&p.id),
                space_id: Some(&sid),
                author_id: "dave",
                block_key: None,
                prompt: "x",
                busy_window_ms: WINDOW,
            },
        )
        .unwrap()
        .unwrap();
        store::finish_draft(
            &db,
            &d2.id,
            "ready",
            Some("s"),
            Some("{}"),
            None,
            None,
            None,
            1,
        )
        .unwrap();
        let d2 = d2.id;
        store::patch(&db, &p.id, "bob", &on(false), &limits)
            .unwrap()
            .unwrap();
        assert_eq!(
            store::apply_draft(&db, &d2, "dave", &version("dave", "x"), &limits).unwrap(),
            Err(store::ApplyError::Denied(store::Denied::Forbidden)),
            "rights are checked before the head"
        );
        assert!(store::draft(&db, &d2).unwrap().is_some());
        // Someone else can't apply it; the author can, once rights return.
        assert_eq!(
            store::apply_draft(&db, &d2, "alice", &version("alice", "x"), &limits).unwrap(),
            Err(store::ApplyError::Gone)
        );
        store::patch(&db, &p.id, "bob", &on(true), &limits)
            .unwrap()
            .unwrap();
        db.lock()
            .execute(
                "UPDATE page_drafts SET base_version = 2 WHERE id = ?1",
                [&d2],
            )
            .unwrap();
        store::patch(&db, &p.id, "bob", &on(false), &limits)
            .unwrap()
            .unwrap();
        assert_eq!(
            store::apply_draft(&db, &d2, "dave", &version("dave", "x"), &limits).unwrap(),
            Err(store::ApplyError::Denied(store::Denied::Forbidden))
        );
        assert!(store::draft(&db, &d2).unwrap().is_some(), "kept on refusal");
        store::patch(&db, &p.id, "bob", &on(true), &limits)
            .unwrap()
            .unwrap();
        assert_eq!(
            store::apply_draft(&db, &d2, "dave", &version("dave", "x"), &limits).unwrap(),
            Ok(p.id.clone())
        );
        assert_eq!(
            store::apply_draft(&db, &d2, "dave", &version("dave", "x"), &limits).unwrap(),
            Err(store::ApplyError::Gone)
        );
        // A discarded draft can't be applied.
        let d3 = new_draft(&db);
        assert!(store::discard_draft(&db, &d3, "bob").unwrap());
        assert_eq!(
            store::apply_draft(&db, &d3, "bob", &version("bob", "x"), &limits).unwrap(),
            Err(store::ApplyError::Gone)
        );
    }

    #[test]
    fn every_mutation_checks_rights_in_its_transaction() {
        let (db, sid) = db_with_space();
        let limits = PageLimits::default();
        db.add_member(&sid, "dave", 10).unwrap();
        let p = store::create(&db, Some(&sid), &version("bob", "B"), &limits)
            .unwrap()
            .unwrap();
        let prefs = store::Prefs {
            pinned: true,
            position: None,
        };
        let icon = store::PatchOps {
            icon: Some(Some("x".to_owned())),
            ..Default::default()
        };
        let members = store::PatchOps {
            members_edit: Some(true),
            ..Default::default()
        };
        // A member without rights: 403 for edits, fine for their own prefs.
        assert_eq!(
            store::patch(&db, &p.id, "dave", &icon, &limits).unwrap(),
            Err(store::PatchError::Denied(store::Denied::Forbidden))
        );
        assert_eq!(
            store::patch(&db, &p.id, "dave", &members, &limits).unwrap(),
            Err(store::PatchError::Denied(store::Denied::Forbidden))
        );
        assert_eq!(
            store::soft_delete(&db, &p.id, "dave").unwrap(),
            Err(store::Denied::Forbidden)
        );
        assert_eq!(
            store::put_prefs(&db, "dave", &p.id, &prefs).unwrap(),
            Ok(())
        );
        // An outsider: 404 for everything, prefs included.
        assert_eq!(
            store::put_prefs(&db, "carol", &p.id, &prefs).unwrap(),
            Err(store::Denied::Gone)
        );
        assert_eq!(
            store::patch(&db, &p.id, "carol", &icon, &limits).unwrap(),
            Err(store::PatchError::Denied(store::Denied::Gone))
        );
        assert_eq!(
            store::soft_delete(&db, &p.id, "carol").unwrap(),
            Err(store::Denied::Gone)
        );
        // A combined rename + members_edit is all or nothing.
        let both = store::PatchOps {
            rename: Some((1, version("dave", "Renamed"))),
            members_edit: Some(true),
            ..Default::default()
        };
        store::patch(&db, &p.id, "bob", &members, &limits)
            .unwrap()
            .unwrap();
        let both_dave = store::PatchOps {
            members_edit: Some(false),
            ..both
        };
        assert!(
            store::patch(&db, &p.id, "dave", &both_dave, &limits)
                .unwrap()
                .is_err()
        );
        assert_eq!(
            store::get(&db, &p.id).unwrap().unwrap().head_version,
            1,
            "the rename wasn't written"
        );
        // Drafts for a page need edit rights now; a gone page is 404.
        let draft_for = |who: &str| {
            store::create_draft(
                &db,
                &store::NewDraft {
                    page_id: Some(&p.id),
                    space_id: Some(&sid),
                    author_id: who,
                    block_key: None,
                    prompt: "x",
                    busy_window_ms: WINDOW,
                },
            )
            .unwrap()
        };
        store::patch(
            &db,
            &p.id,
            "bob",
            &store::PatchOps {
                members_edit: Some(false),
                ..Default::default()
            },
            &limits,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            draft_for("dave").unwrap_err(),
            store::DraftError::Denied(store::Denied::Forbidden)
        );
        assert_eq!(
            draft_for("carol").unwrap_err(),
            store::DraftError::Denied(store::Denied::Gone)
        );
        store::soft_delete(&db, &p.id, "bob").unwrap().unwrap();
        assert_eq!(
            draft_for("bob").unwrap_err(),
            store::DraftError::Denied(store::Denied::Gone)
        );
        assert_eq!(
            store::put_prefs(&db, "bob", &p.id, &prefs).unwrap(),
            Err(store::Denied::Gone)
        );
        assert_eq!(
            store::add_version(&db, &p.id, 1, &version("bob", "x"), &limits).unwrap(),
            Err(store::SaveError::Denied(store::Denied::Gone))
        );
    }

    fn compiled(db: &Db, user: &str, space: Option<&str>, src: &str) -> NewVersion {
        let mut r = super::super::access::DbResolver {
            db,
            user,
            space: space.map(str::to_owned),
        };
        let c = super::super::compile::compile(src, &mut r);
        let spec = c.spec.unwrap_or_else(|| panic!("{:#?}", c.diagnostics));
        NewVersion {
            author_id: user.to_owned(),
            kind: "manual",
            prompt: None,
            model_ref: None,
            restored_from: None,
            source: c.source,
            spec: serde_json::to_string(&spec).unwrap(),
            title: spec.title,
        }
    }

    #[test]
    fn a_superseded_click_writes_nothing() {
        let (db, sid) = db_with_space();
        let limits = PageLimits::default();
        let src = r#"export default <Page title="P"><Button key="ask" label="Ask" action="runAgent" agent="agent" room="lobby" prompt="Go"/></Page>"#;
        let p = store::create(
            &db,
            Some(&sid),
            &compiled(&db, "alice", Some(&sid), src),
            &limits,
        )
        .unwrap()
        .unwrap();
        store::add_version(
            &db,
            &p.id,
            1,
            &compiled(&db, "alice", Some(&sid), src),
            &limits,
        )
        .unwrap()
        .unwrap();
        let author = crate::db::Author {
            id: Some("alice".into()),
            name: "Alice".into(),
        };
        let req = store::ActionRequest {
            page: &p.id,
            user: "alice",
            author: &author,
            version: 1,
            block_key: "ask",
            confirm: true,
            schedule_revision: None,
            chat_turn: "chat_x",
            max_prompt_chars: 8_000,
        };
        assert!(matches!(
            store::claim_action(&db, &req).unwrap(),
            Err(store::ActionError::Conflict { head: 2 })
        ));
        let count = |table: &str| -> i64 {
            db.lock()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count("space_events"), 0, "no trigger");
        assert_eq!(count("page_runs"), 0);
        assert_eq!(count("page_confirmations"), 0, "no consent recorded");
        // The current version claims once, with everything in one go.
        let req = store::ActionRequest { version: 2, ..req };
        assert!(matches!(
            store::claim_action(&db, &req).unwrap(),
            Ok(store::Claimed::Room { .. })
        ));
        assert_eq!(count("space_events"), 1);
        assert_eq!(count("page_runs"), 1);
        // A Bob who is gone gets 404 and writes nothing.
        db.remove_member(&sid, "bob").unwrap();
        let bob = crate::db::Author {
            id: Some("bob".into()),
            name: "Bob".into(),
        };
        let req = store::ActionRequest {
            user: "bob",
            author: &bob,
            ..req
        };
        assert!(matches!(
            store::claim_action(&db, &req).unwrap(),
            Err(store::ActionError::Denied(store::Denied::Gone))
        ));
        assert_eq!(count("space_events"), 1);
    }

    #[test]
    fn threads_from_the_database_are_never_button_destinations() {
        let (db, sid) = db_with_space();
        db.create_room(&sid, "deploys", Some("lobby"), 100, 100)
            .unwrap();
        let mut r = super::super::access::DbResolver {
            db: &db,
            user: "alice",
            space: Some(sid.clone()),
        };
        let src = r#"export default <Page title="P"><Button key="b" label="x" action="runAgent" agent="agent" room="deploys" prompt="Go"/></Page>"#;
        let c = super::super::compile::compile(src, &mut r);
        assert!(
            c.diagnostics.iter().any(|d| d.message.contains("thread")),
            "{:#?}",
            c.diagnostics
        );
    }

    #[test]
    fn version_and_run_reads_carry_the_viewers_grant() {
        let (db, sid) = db_with_space();
        let limits = PageLimits::default();
        let p = store::create(&db, Some(&sid), &version("alice", "v1"), &limits)
            .unwrap()
            .unwrap();
        db.lock()
            .execute(
                "INSERT INTO page_runs (page_id, version, block_key, user_id, trigger_id, run_id, \
                 error, created_at) VALUES (?1, 1, 'ask', 'alice', 1, '', 'no model', 0)",
                [&p.id],
            )
            .unwrap();
        assert!(store::version_list_for(&db, &p.id, "bob").unwrap().is_ok());
        assert!(
            store::latest_run(&db, "bob", &p.id, "ask", None)
                .unwrap()
                .is_some()
        );
        // Bob is removed; then Alice writes version 2.
        db.remove_member(&sid, "bob").unwrap();
        store::add_version(&db, &p.id, 1, &version("alice", "v2 secret"), &limits)
            .unwrap()
            .unwrap();
        assert_eq!(
            store::version_list_for(&db, &p.id, "bob").unwrap(),
            Err(store::Denied::Gone)
        );
        for ns in [vec![1], vec![2], vec![0], vec![1, 2]] {
            assert_eq!(
                store::versions_for(&db, &p.id, "bob", &ns).unwrap(),
                Err(store::Denied::Gone)
            );
        }
        assert!(
            store::latest_run(&db, "bob", &p.id, "ask", None)
                .unwrap()
                .is_none()
        );
        // Alice still reads everything.
        let (head, list) = store::version_list_for(&db, &p.id, "alice")
            .unwrap()
            .unwrap();
        assert_eq!((head, list.len()), (2, 2));
        let found = store::versions_for(&db, &p.id, "alice", &[0])
            .unwrap()
            .unwrap();
        assert_eq!(found[0].as_ref().unwrap().version, 2);
    }

    #[test]
    fn confirmation_describes_live_destinations_and_the_claimed_schedule_revision() {
        use serde_json::json;

        let (db, sid) = db_with_space();
        let agent = db.default_space_agent(&sid).unwrap().unwrap();
        let task = crate::schedules::create_task(
            &db,
            "alice",
            crate::schedules::CreateBody {
                name: "Nightly".into(),
                prompt: "Old task prompt".into(),
                space_id: Some(sid.clone()),
                room_id: Some("lobby".into()),
                schedule: crate::schedules::ScheduleIn {
                    kind: "cron".into(),
                    at: None,
                    expression: Some("0 3 * * *".into()),
                    timezone: Some("UTC".into()),
                },
                enabled: true,
            },
            chrono::Utc::now(),
            8_000,
        )
        .unwrap();
        let source = format!(
            r#"export default <Page title="P">
                <Button key="ask" label="Ask" action="runAgent" agent="agent" room="lobby" prompt="Go"/>
                <Button key="run" label="Run" action="runSchedule" schedule="{}"/>
            </Page>"#,
            task.id
        );
        let page = store::create(
            &db,
            Some(&sid),
            &compiled(&db, "bob", Some(&sid), &source),
            &PageLimits::default(),
        )
        .unwrap()
        .unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE space_agents SET name = 'Fresh name', handle = 'fresh' WHERE id = ?1",
                [&agent.id],
            )
            .unwrap();
            conn.execute(
                "UPDATE space_rooms SET name = 'Fresh room' WHERE space_id = ?1 AND id = 'lobby'",
                [&sid],
            )
            .unwrap();
        }
        let author = crate::db::Author {
            id: Some("alice".into()),
            name: "Alice".into(),
        };
        let mut req = store::ActionRequest {
            page: &page.id,
            user: "alice",
            author: &author,
            version: 1,
            block_key: "ask",
            confirm: false,
            schedule_revision: None,
            chat_turn: "chat_x",
            max_prompt_chars: 8_000,
        };
        let Err(store::ActionError::NeedsConfirmation(agent_challenge)) =
            store::claim_action(&db, &req).unwrap()
        else {
            panic!("expected agent challenge");
        };
        let action = &agent_challenge["action"];
        assert_eq!(action["agent"], "fresh");
        assert_eq!(action["agent_name"], "Fresh name");
        assert_eq!(action["agent_id"], agent.id);
        assert_eq!(action["room"], "Fresh room");
        assert_eq!(action["room_id"], "lobby");
        assert_eq!(action["space_id"], sid);

        req.block_key = "run";
        let Err(store::ActionError::NeedsConfirmation(schedule_challenge)) =
            store::claim_action(&db, &req).unwrap()
        else {
            panic!("expected schedule challenge");
        };
        let action = &schedule_challenge["action"];
        assert_eq!(action["room"], "Fresh room");
        assert_eq!(action["room_id"], "lobby");
        assert_eq!(action["schedule_revision"], json!(task.revision));
        assert_eq!(action["prompt"], "Old task prompt");
        let new_room = db
            .create_room(&sid, "New task room", None, 100, 100)
            .unwrap();
        db.lock()
            .execute(
                "UPDATE schedules SET revision = revision + 1, prompt = 'New task prompt',
                 room_id = ?2 WHERE id = ?1",
                [&task.id, &new_room.id],
            )
            .unwrap();
        req.confirm = true;
        req.schedule_revision = Some(task.revision);
        assert!(matches!(
            store::claim_action(&db, &req).unwrap(),
            Err(store::ActionError::ScheduleChanged { revision }) if revision == task.revision + 1
        ));
        for table in ["page_confirmations", "schedule_runs"] {
            let count: i64 = db
                .lock()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
        req.confirm = false;
        let Err(store::ActionError::NeedsConfirmation(new_challenge)) =
            store::claim_action(&db, &req).unwrap()
        else {
            panic!("expected a fresh schedule challenge");
        };
        assert_eq!(new_challenge["action"]["prompt"], "New task prompt");
        assert_eq!(new_challenge["action"]["room"], "New task room");
        assert_eq!(new_challenge["action"]["room_id"], new_room.id);
        req.confirm = true;
        req.schedule_revision = new_challenge["action"]["schedule_revision"].as_i64();
        let Ok(store::Claimed::Schedule { task: claimed, .. }) =
            store::claim_action(&db, &req).unwrap()
        else {
            panic!("expected a schedule claim");
        };
        assert_eq!(claimed.revision, task.revision + 1);
        assert_eq!(claimed.prompt, "New task prompt");
        assert_eq!(claimed.target, Some((sid, new_room.id)));
    }

    #[test]
    fn schedules_in_threads_never_run_from_a_page() {
        let (db, sid) = db_with_space();
        let thread = db
            .create_room(&sid, "nightly-thread", Some("lobby"), 100, 100)
            .unwrap();
        let body = crate::schedules::CreateBody {
            name: "N".into(),
            prompt: "p".into(),
            space_id: Some(sid.clone()),
            room_id: Some(thread.id.clone()),
            schedule: crate::schedules::ScheduleIn {
                kind: "cron".into(),
                at: None,
                expression: Some("0 3 * * *".into()),
                timezone: Some("UTC".into()),
            },
            enabled: true,
        };
        let task =
            crate::schedules::create_task(&db, "alice", body, chrono::Utc::now(), 8_000).unwrap();
        let src = format!(
            r#"export default <Page title="P"><Button key="run" label="x" action="runSchedule" schedule="{}"/></Page>"#,
            task.id
        );
        let limits = PageLimits::default();
        let p = store::create(
            &db,
            Some(&sid),
            &compiled(&db, "alice", Some(&sid), &src),
            &limits,
        )
        .unwrap()
        .unwrap();
        let spec: super::super::spec::PageSpec =
            serde_json::from_str(&store::version(&db, &p.id, 1).unwrap().unwrap().spec).unwrap();
        let a = spec.block("run").unwrap().action.clone().unwrap();
        // A legacy personal page with a Space-room button: disabled, with why.
        let mut legacy = a.clone();
        legacy.kind = "runAgent".into();
        legacy.target.space_id = Some(sid.clone());
        let Err(e) = store::destination_in(&db.lock(), None, &legacy, "alice").unwrap() else {
            panic!("a personal page's Space button was allowed");
        };
        assert!(
            e.reason().contains("only start private runs"),
            "{}",
            e.reason()
        );
        let dest = store::destination_in(&db.lock(), Some(&sid), &a, "alice").unwrap();
        let Err(e) = dest else {
            panic!("a thread schedule was allowed")
        };
        assert!(e.reason().contains("thread"), "{}", e.reason());
        let author = crate::db::Author {
            id: Some("alice".into()),
            name: "Alice".into(),
        };
        let req = store::ActionRequest {
            page: &p.id,
            user: "alice",
            author: &author,
            version: 1,
            block_key: "run",
            confirm: true,
            schedule_revision: Some(task.revision),
            chat_turn: "chat_x",
            max_prompt_chars: 8_000,
        };
        assert!(matches!(
            store::claim_action(&db, &req).unwrap(),
            Err(store::ActionError::Forbidden(_))
        ));
        let runs: i64 = db
            .lock()
            .query_row("SELECT COUNT(*) FROM schedule_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(runs, 0);
    }

    #[test]
    fn discard_claims_only_the_authors_live_draft() {
        let db = Db::open_in_memory().unwrap();
        let d = store::create_draft(&db, &draft("alice", WINDOW))
            .unwrap()
            .unwrap();
        assert!(
            !store::discard_draft(&db, &d.id, "bob").unwrap(),
            "not Bob's"
        );
        assert!(store::draft(&db, &d.id).unwrap().is_some());
        db.lock()
            .execute(
                "UPDATE page_drafts SET expires_at = 0 WHERE id = ?1",
                [&d.id],
            )
            .unwrap();
        assert!(
            !store::discard_draft(&db, &d.id, "alice").unwrap(),
            "expired"
        );
        assert!(!store::discard_draft(&db, "pgd_nope", "alice").unwrap());
        let d = store::create_draft(&db, &draft("alice", WINDOW))
            .unwrap()
            .unwrap();
        assert!(store::discard_draft(&db, &d.id, "alice").unwrap());
        assert!(
            !store::discard_draft(&db, &d.id, "alice").unwrap(),
            "already discarded"
        );
    }

    #[test]
    fn tables_from_the_earlier_lane_schema_are_upgraded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.sqlite");
        let page_id = {
            let db = Db::open(&path).unwrap();
            let space = db.create_space("alice", "Team", 10).unwrap();
            let p = store::create(
                &db,
                Some(&space.id),
                &version("alice", "Old"),
                &PageLimits::default(),
            )
            .unwrap()
            .unwrap();
            // Recreate the three tables exactly as the lane's first build did.
            db.lock()
                .execute_batch(&format!(
                    "PRAGMA foreign_keys = OFF;
                     DROP TABLE page_versions; DROP TABLE page_drafts; DROP TABLE page_runs;
                     CREATE TABLE page_versions (
                         page_id TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
                         version INTEGER NOT NULL, author_id TEXT NOT NULL,
                         kind TEXT NOT NULL CHECK (kind IN ('create', 'prompt', 'manual', 'restore', 'template')),
                         prompt TEXT, model_ref TEXT, restored_from INTEGER,
                         source TEXT NOT NULL, spec TEXT NOT NULL, created_at INTEGER NOT NULL,
                         PRIMARY KEY (page_id, version));
                     INSERT INTO page_versions VALUES ('{0}', 1, 'alice', 'create', NULL, NULL, NULL, 's', '{{}}', 0);
                     CREATE TABLE page_drafts (
                         id TEXT PRIMARY KEY NOT NULL, page_id TEXT REFERENCES pages (id) ON DELETE CASCADE,
                         scope TEXT NOT NULL CHECK (scope IN ('personal', 'space')),
                         space_id TEXT REFERENCES spaces (id) ON DELETE CASCADE, author_id TEXT NOT NULL,
                         base_version INTEGER, block_key TEXT, prompt TEXT NOT NULL,
                         status TEXT NOT NULL CHECK (status IN ('generating', 'ready', 'failed')),
                         source TEXT, spec TEXT, diagnostics TEXT, error TEXT, model_ref TEXT,
                         attempts INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL,
                         expires_at INTEGER NOT NULL);
                     CREATE INDEX page_drafts_author ON page_drafts (author_id, created_at);
                     INSERT INTO page_drafts (id, scope, author_id, prompt, status, created_at, expires_at)
                         VALUES ('pgd_old', 'personal', 'alice', 'p', 'ready', 0, 9999999999999);
                     CREATE TABLE page_runs (
                         id INTEGER PRIMARY KEY AUTOINCREMENT,
                         page_id TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
                         version INTEGER NOT NULL, block_key TEXT NOT NULL, user_id TEXT NOT NULL,
                         space_id TEXT, room_id TEXT, trigger_id INTEGER NOT NULL,
                         run_id TEXT NOT NULL, created_at INTEGER NOT NULL);
                     PRAGMA foreign_keys = ON;",
                    p.id
                ))
                .unwrap();
            p.id
        };
        // Opened by this build: upgraded in place, rows kept, twice over;
        // the rebuilt table's index is there right after the first open.
        for _ in 0..2 {
            let db = Db::open(&path).unwrap();
            let indexed: bool = db
                .lock()
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master
                     WHERE type = 'index' AND name = 'page_drafts_author'
                       AND tbl_name = 'page_drafts')",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(indexed, "page_drafts_author is missing after the upgrade");
            assert_eq!(
                store::version_list_for(&db, &page_id, "alice")
                    .unwrap()
                    .unwrap()
                    .1
                    .len(),
                1
            );
            assert!(store::draft(&db, "pgd_old").unwrap().is_some());
        }
        let db = Db::open(&path).unwrap();
        store::add_version(
            &db,
            &page_id,
            1,
            &NewVersion {
                kind: "rename",
                ..version("alice", "R")
            },
            &PageLimits::default(),
        )
        .unwrap()
        .unwrap();
        let d = store::create_draft(&db, &draft("alice", WINDOW))
            .unwrap()
            .unwrap();
        assert!(store::discard_draft(&db, &d.id, "alice").unwrap());
        db.lock()
            .execute(
                "INSERT INTO page_runs (page_id, version, block_key, user_id, trigger_id, run_id, error, created_at)
                 VALUES (?1, 1, 'b', 'alice', 1, 'x', 'e', 0)",
                [&page_id],
            )
            .unwrap();
    }

    #[test]
    fn a_restart_marks_orphaned_page_runs_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runs.sqlite");
        let page_id = {
            let db = Db::open(&path).unwrap();
            let p = store::create(&db, None, &version("alice", "Mine"), &PageLimits::default())
                .unwrap()
                .unwrap();
            let insert = |run: &str| {
                db.lock()
                    .execute(
                        "INSERT INTO page_runs (page_id, version, block_key, user_id, trigger_id, \
                         run_id, created_at) VALUES (?1, 1, ?2, 'alice', 1, ?3, 0)",
                        rusqlite::params![p.id, run, run],
                    )
                    .unwrap();
            };
            insert(""); // claimed, never got a run id
            insert("chat_never"); // page-only, never started
            insert("chat_started"); // page-only, started, never ended
            let started = crate::db::Activity::agent("chat_started", "Metro").at("started");
            store::append_page_event(
                &db,
                &p.id,
                "alice",
                "activity",
                &crate::db::Author::server("agent"),
                "",
                Some(&started),
            )
            .unwrap();
            p.id
        };
        let db = Db::open(&path).unwrap();
        let errors: Vec<(String, Option<String>)> = {
            let conn = db.lock();
            let mut stmt = conn
                .prepare("SELECT run_id, error FROM page_runs ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(errors[0].1.as_deref(), Some(store::RUN_INTERRUPTED));
        assert_eq!(errors[1].1.as_deref(), Some(store::RUN_INTERRUPTED));
        assert_eq!(
            errors[2].1, None,
            "a started run is ended by its marker instead"
        );
        let last =
            store::last_page_event(&db, &page_id, "alice", &["activity"], Some("chat_started"))
                .unwrap()
                .unwrap();
        assert_eq!(last.activity.unwrap().phase.as_deref(), Some("interrupted"));
        // Nothing is replayed: no new triggers.
        let triggers: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM page_events WHERE kind = 'message'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(triggers, 0);
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
        store::soft_delete(&db, &p.id, "alice").unwrap().unwrap();
        let now = crate::db::now_ms();
        assert_eq!(store::purge_expired(&db, now).unwrap(), 0);
        assert_eq!(
            store::purge_expired(&db, now + store::DELETED_RETENTION_MS + 1).unwrap(),
            1
        );
        assert!(store::get(&db, &p.id).unwrap().is_none());
    }
}

/// A caller that goes away at any point never strands claimed work.
mod cancellation {
    use std::sync::Arc;
    use std::time::Duration;

    use super::super::limits::PageLimits;
    use super::super::{actions, api, store};
    use crate::auth::{AuthFailure, AuthFuture, Human, HumanAuth};
    use crate::db::Db;
    use crate::{AppState, Config, Limits};

    struct NoAuth;

    impl HumanAuth for NoAuth {
        fn identity<'a>(&'a self, _bearer: &'a str) -> AuthFuture<'a> {
            Box::pin(async { Err(AuthFailure::Rejected("tests".to_owned())) })
        }
        fn namespace(&self) -> &str {
            "dev"
        }
    }

    async fn state() -> AppState {
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            // No model at all: work that starts fails quickly and visibly.
            model: None,
            limits: Limits::default(),
            org_admins: Vec::new(),
        };
        AppState::new(config, Db::open_in_memory().unwrap(), Arc::new(NoAuth))
            .await
            .unwrap()
    }

    fn hold_lock(db: &Db) -> std::sync::mpsc::Sender<()> {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (locked, is_locked) = std::sync::mpsc::channel::<()>();
        let db = db.clone();
        std::thread::spawn(move || {
            let _guard = db.lock();
            locked.send(()).unwrap();
            let _ = wait.recv();
        });
        is_locked.recv().unwrap();
        release
    }

    fn alice() -> Human {
        Human {
            id: "alice".into(),
            name: "Alice".into(),
        }
    }

    #[tokio::test]
    async fn a_click_cancelled_while_its_claim_is_pending_still_runs_and_records() {
        let st = state().await;
        let space = st.db.create_space("alice", "Team", 10).unwrap();
        let src = r#"export default <Page title="P"><Button key="ask" label="Ask" action="runAgent" agent="agent" room="lobby" prompt="Go"/></Page>"#;
        let mut r = super::super::access::DbResolver {
            db: &st.db,
            user: "alice",
            space: Some(space.id.clone()),
        };
        let c = super::super::compile::compile(src, &mut r);
        let spec = c.spec.unwrap();
        let v = store::NewVersion {
            author_id: "alice".into(),
            kind: "manual",
            prompt: None,
            model_ref: None,
            restored_from: None,
            source: c.source,
            spec: serde_json::to_string(&spec).unwrap(),
            title: spec.title,
        };
        let p = store::create(&st.db, Some(&space.id), &v, &PageLimits::default())
            .unwrap()
            .unwrap();
        let release = hold_lock(&st.db);
        let (s2, pid) = (st.clone(), p.id.clone());
        let caller = tokio::spawn(async move {
            let click = actions::Click {
                version: 1,
                block_key: "ask".into(),
                confirm: false,
                schedule_revision: None,
            };
            let _ = actions::run(&s2, &alice(), &pid, click).await;
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        caller.abort();
        let _ = caller.await;
        drop(release);
        for _ in 0..300 {
            let row: Option<(String, Option<String>)> = {
                let conn = st.db.lock();
                rusqlite::OptionalExtension::optional(conn.query_row(
                    "SELECT run_id, error FROM page_runs",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ))
                .unwrap()
            };
            if let Some((run_id, error)) = row
                && (!run_id.is_empty() || error.is_some())
            {
                // No model: the run couldn't start, and that's recorded.
                assert!(error.is_some(), "{run_id:?}");
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the claimed click was never dispatched or recorded");
    }

    #[tokio::test]
    async fn data_checks_the_viewers_grant_with_the_destination() {
        let st = state().await;
        let space = st.db.create_space("alice", "Team", 10).unwrap();
        st.db.add_member(&space.id, "bob", 10).unwrap();
        let src = r#"export default <Page title="P"><Button key="ask" label="Ask" action="runAgent" agent="agent" room="lobby" prompt="Go"/></Page>"#;
        let mut r = super::super::access::DbResolver {
            db: &st.db,
            user: "alice",
            space: Some(space.id.clone()),
        };
        let c = super::super::compile::compile(src, &mut r);
        let spec = c.spec.unwrap();
        let ctx = |pid: &str| super::super::data::Context {
            page_id: Some(pid.to_owned()),
            space_id: Some(space.id.clone()),
            version: Some(1),
            draft: false,
        };
        let v = store::NewVersion {
            author_id: "alice".into(),
            kind: "manual",
            prompt: None,
            model_ref: None,
            restored_from: None,
            source: c.source.clone(),
            spec: serde_json::to_string(&spec).unwrap(),
            title: spec.title.clone(),
        };
        let p = store::create(&st.db, Some(&space.id), &v, &PageLimits::default())
            .unwrap()
            .unwrap();
        let out = super::super::data::resolve(&st, ctx(&p.id), spec.clone(), "bob".into(), None)
            .await
            .unwrap();
        assert_eq!(out["actions"]["ask"]["enabled"], true);
        // Bob is removed between the page check and the data read.
        st.db.remove_member(&space.id, "bob").unwrap();
        let out = super::super::data::resolve(&st, ctx(&p.id), spec, "bob".into(), None)
            .await
            .unwrap();
        assert_eq!(out["actions"]["ask"]["enabled"], false, "{out}");
        // And no room metadata without membership.
        assert!(
            store::room_for(&st.db, "bob", &space.id, "lobby")
                .unwrap()
                .is_none()
        );
        assert!(
            store::room_for(&st.db, "alice", &space.id, "lobby")
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_draft_cancelled_while_its_reservation_is_pending_still_starts() {
        let st = state().await;
        let release = hold_lock(&st.db);
        let s2 = st.clone();
        let caller = tokio::spawn(async move {
            let request = api::DraftRequest {
                user: "alice".into(),
                page_id: None,
                space: None,
                block_key: None,
                prompt: "x".into(),
                chosen: None,
            };
            let _ = api::start_draft(&s2, request).await;
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        caller.abort();
        let _ = caller.await;
        drop(release);
        for _ in 0..300 {
            let status: Option<String> = {
                let conn = st.db.lock();
                rusqlite::OptionalExtension::optional(conn.query_row(
                    "SELECT status FROM page_drafts",
                    [],
                    |r| r.get(0),
                ))
                .unwrap()
            };
            if let Some(s) = status
                && s != "generating"
            {
                // No model: the job ran and failed visibly, freeing the slot.
                assert_eq!(s, "failed");
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the reserved draft's job never ran");
    }
}

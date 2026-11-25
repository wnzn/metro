import { Action, Badge, Column, Label, Row } from "./primitives";
import { DiffView, Editor, Progress, TextView } from "nsh/base";
import type { EditorDiagnostic, TextViewDocument } from "nsh/base";
import { Bar, Line, Plot, PlotAxis } from "nsh/base/plot";
import { effect, keymap, Scroll, signal, View } from "nsh";
import type { JSX, ScrollRef } from "nsh";
import { cx, toneSolidClass } from "./tokens";
import type { Tone } from "./tokens";
import { httpsLink } from "./pageMarkdown";
export { httpsLink } from "./pageMarkdown";

export function PageBlockFrame(props: { children?: JSX.Element; blockKey?: string; blockLabel?: string; changed?: boolean; onEdit?: () => void }) {
  return (
    <View class={cx("group min-w-0 flex-col gap-1", props.changed && "border border-ring rounded-(--radius-sm) p-2")}
      testID={props.blockKey ? `page-block:${props.blockKey}` : undefined}>
      {props.children}
      <If when={props.onEdit}>
        <Row justify="end">
          <Action compact variant="ghost" icon="pencil" label="Edit block…" accessibilityLabel={`Edit ${props.blockLabel || "block"} with prompt`}
            testID={`page-edit-block:${props.blockKey}`} onPress={() => props.onEdit?.()} />
        </Row>
      </If>
    </View>
  );
}

export function PageGrid(props: { children?: JSX.Element }) {
  return <View class="flex-row flex-wrap items-start gap-2">{props.children}</View>;
}

export function PageGridCell(props: { columns: number; children?: JSX.Element }) {
  return <View class="flex-col min-w-0 shrink-0" style={{ width: `${Math.floor(96 / props.columns)}%` }}>{props.children}</View>;
}

export function PageProgress(props: { percent: number; tone: Tone; label: string }) {
  return <Column gap={1}>
    <Label variant="caption">{props.label} · {Math.round(props.percent)}%</Label>
    <Progress value={props.percent} trackClass="h-[6px] rounded-full bg-muted" indicatorClass={cx("h-full rounded-full", toneSolidClass[props.tone])} accessibilityLabel={props.label} />
  </Column>;
}

export function PageChart(props: { points: { label: string; value: number }[]; kind: "line" | "bar"; label: string }) {
  return <Column gap={1}>
    <Label variant="strong">{props.label}</Label>
    <Plot class="h-[180px] text-tone-system-foreground" data={props.points} y="value" xScale={{ type: props.kind === "bar" ? "band" : "point" }}
      accessibilityLabel={props.label} accessibilityValue={{ text: props.points.map(p => `${p.label}: ${p.value}`).join(", ") }}>
      <PlotAxis placement="left" />
      <PlotAxis placement="bottom" format={(_, i) => props.points[i]?.label ?? ""} />
      <If when={props.kind === "bar"} else={<Line />}><Bar /></If>
    </Plot>
  </Column>;
}

type TableCell = { label: string; tone?: Tone };
export function PageTable(props: { label?: string; headings: string[]; rows: TableCell[][] }) {
  const columnWidth = 112;
  const viewportWidth = signal(0);
  const focused = signal(false);
  let scroll: ScrollRef | undefined, offset = 0;
  const width = () => Math.max(viewportWidth(), props.headings.length * columnWidth);
  const wide = () => viewportWidth() > 0 && width() > viewportWidth();
  effect(() => {
    if (!wide() || !focused()) return;
    const move = (direction: number) => () => {
      offset = Math.max(0, Math.min(width() - viewportWidth(), offset + direction * columnWidth));
      scroll?.scrollTo({ x: offset, animated: false });
    };
    const left = keymap.bind("left", move(-1), { when: "always" });
    const right = keymap.bind("right", move(1), { when: "always" });
    return () => { left(); right(); };
  });
  return <Scroll horizontal focusable={wide()} class="min-w-0 focus:ring-1 focus:ring-ring" role="region"
    accessibilityLabel={wide() ? `${props.label || "Table"}, scroll horizontally` : props.label || "Table"}
    testID="page-table-scroll" onLayout={e => viewportWidth.set(e.bounds.size.width)}
    ref={node => { scroll = node; }} onFocus={() => focused.set(true)} onBlur={() => focused.set(false)}
    onScroll={e => { offset = e.offset.x; }}>
    <View role="table" class="flex-col gap-1" style={{ width: width() }} testID="page-table">
      <View role="row" class="flex-row gap-2 border-b border-border px-2 py-1">
        <Each of={props.headings}>{title => <View role="columnheader" class="flex-1 min-w-0"><Label variant="strong" lines={2}>{title()}</Label></View>}</Each>
      </View>
      <Each of={props.rows}>{(row, index) =>
        <View role="row" class={cx("flex-row gap-2 px-2 py-1", index() % 2 === 0 && "bg-muted")}>
          <Each of={row()}>{value => <View role="cell" class="flex-1 min-w-0">
            <If when={value().tone} else={<Label lines={3}>{value().label}</Label>}>{t => <Row><Badge tone={t()} label={value().label} /></Row>}</If>
          </View>}</Each>
        </View>
      }</Each>
    </View>
  </Scroll>;
}

export function PageMarkdown(props: { document: TextViewDocument; onLink?: (url: string) => void }) {
  return <TextView document={props.document} classes={{ link: "text-accent-foreground underline" }} links="report" selectable onLinkPress={e => {
    const url = httpsLink(e.url);
    if (url) props.onLink?.(url);
  }} />;
}

export function PageCode(props: { value: string; readOnly?: boolean; onChange?: (value: string) => void; diagnostics?: EditorDiagnostic[] }) {
  return <Editor class="min-h-[240px] flex-1 rounded-(--radius-sm) border border-border bg-background"
    testID="page-code" accessibilityLabel="Page TSX source" language="tsx" value={props.value} readOnly={props.readOnly} autoFocus={!props.readOnly}
    onChange={props.onChange ? e => props.onChange?.(e.value) : undefined} diagnostics={props.diagnostics} scrollBeyondLastLine={false} />;
}

export function PageDiff(props: { before: string; after: string; accessibilityLabel?: string }) {
  return <View class="min-h-[260px] flex-1" role="region" accessibilityLabel={props.accessibilityLabel ?? "Source changes"}>
    <DiffView class="min-h-[260px] flex-1 rounded-(--radius-sm) border border-border bg-background"
      testID="page-source-diff" language="tsx" layout="unified" oldValue={props.before} newValue={props.after} />
  </View>;
}

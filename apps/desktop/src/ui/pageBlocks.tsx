import { Action, Badge, Column, Label, Row } from "./primitives";
import { DiffView, Editor, Progress, TextView } from "nsh/base";
import type { EditorDiagnostic, TextViewDocument } from "nsh/base";
import { Bar, Line, Plot, PlotAxis } from "nsh/base/plot";
import { effect, keymap, Scroll, signal, Text, View } from "nsh";
import type { JSX, ScrollRef } from "nsh";
import { cx, textClass, toneSolidClass } from "./tokens";
import type { Tone } from "./tokens";
import { httpsLink } from "./pageMarkdown";
export { httpsLink } from "./pageMarkdown";

export function PageEditPanel(props: { testID?: string; children?: JSX.Element }) {
  return <View class="flex-col gap-(--space-2) rounded-(--radius-md) bg-muted p-(--space-3)" testID={props.testID}>{props.children}</View>;
}

export function PageBlockFrame(props: { children?: JSX.Element; blockKey?: string; blockLabel?: string; highlight?: "Added" | "Changed"; onEdit?: () => void }) {
  return (
    <View class={cx("group min-w-0 flex-col gap-(--space-1)", props.highlight && "rounded-(--radius-sm) bg-accent/50 p-(--space-2)")}
      testID={props.blockKey ? `page-block:${props.blockKey}` : undefined}>
      <If when={props.highlight}><Label variant="meta" color="accent">{props.highlight}</Label></If>
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
    <Label heading variant="heading">{props.label}</Label>
    <View class="pl-(--space-8) pb-(--space-6)">
      <Plot class="h-[160px] text-foreground" data={props.points} y="value"
        xScale={props.kind === "bar" ? { type: "band", maxBandWidth: 48, paddingInner: 0.24, paddingOuter: 0.12 } : { type: "point" }}
        accessibilityLabel={props.label} accessibilityValue={{ text: props.points.map(p => `${p.label}: ${p.value}`).join(", ") }}>
        <PlotAxis placement="left" ticks={4} labelClass={cx(textClass.xs, "text-muted-foreground tabular-nums")} lineClass="w-px bg-border" />
        <PlotAxis placement="bottom" ticks={6} format={i => props.points[i]?.label ?? ""} labelClass={cx(textClass.xs, "text-muted-foreground")} lineClass="h-px bg-border" />
        <If when={props.kind === "bar"} else={<Line class="stroke-ring" />}><Bar class="fill-ring" /></If>
      </Plot>
    </View>
  </Column>;
}

type TableCell = { label: string; tone?: Tone };
export function PageTable(props: { label?: string; headings: string[]; rows: TableCell[][]; numericColumns?: boolean[] }) {
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
    <View role="table" class="flex-col" style={{ width: width() }} testID="page-table">
      <View role="row" class="flex-row items-center gap-(--space-3) border-b border-border px-(--space-2) py-(--space-2)">
        <Each of={props.headings}>{(title, i) => <View role="columnheader" class="flex-1 min-w-0">
          <Text class={cx(textClass.sm, "font-semibold text-muted-foreground", props.numericColumns?.[i()] && "text-right")}
            style={{ numberOfLines: 2 }}>{title()}</Text>
        </View>}</Each>
      </View>
      <Each of={props.rows}>{row =>
        <View role="row" class="flex-row items-center gap-(--space-3) border-b border-border px-(--space-2) py-(--space-2)">
          <Each of={row()}>{(value, i) => <View role="cell" class="flex-1 min-w-0">
            <If when={value().tone} else={
              <Text class={cx(textClass.sm, "text-foreground", props.numericColumns?.[i()] && "text-right tabular-nums")}
                style={{ numberOfLines: 3 }}>{value().label}</Text>
            }>{t => <Row><Badge tone={t()} label={value().label} /></Row>}</If>
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

export function PageDiff(props: { before: string; after: string; beforeLabel?: string; afterLabel?: string; accessibilityLabel?: string }) {
  return <View class="min-h-[260px] flex-1 flex-col gap-(--space-2)" role="region" accessibilityLabel={props.accessibilityLabel ?? "Source changes"}>
    <If when={props.beforeLabel && props.afterLabel}>
      <Row gap={3} wrap>
        <Label variant="meta">Before: {props.beforeLabel}</Label>
        <Label variant="meta">After: {props.afterLabel}</Label>
      </Row>
    </If>
    <DiffView class="min-h-[260px] flex-1 rounded-(--radius-sm) border border-border bg-background"
      testID="page-source-diff" language="tsx" layout="unified" oldValue={props.before} newValue={props.after} />
  </View>;
}

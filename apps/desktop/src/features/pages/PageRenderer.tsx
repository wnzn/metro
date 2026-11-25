import { computed, signal } from "nsh";
import type { ActionKind, PageData, PageSpec } from "../../api/pages";
import { Action, Badge, Column, Disclosure, Divider, Label, Row, Surface } from "../../ui";
import type { Tone } from "../../ui";
import { PageBlockFrame, PageChart, PageGrid, PageGridCell, PageMarkdown, PageProgress, PageTable } from "../../ui/pageBlocks";
import { capabilityOf, cell, number, own, planPage, str } from "./renderPlan";
import type { PlanBlock } from "./renderPlan";

type Interactions = {
  data: PageData | null;
  readOnly?: boolean;
  busy?: boolean;
  changed?: ReadonlySet<string>;
  onAction?: (key: string, kind: ActionKind) => void;
  onEdit?: (key: string) => void;
  onLink?: (url: string) => void;
  onFile?: (id: string) => void;
  onSchedule?: (block: string) => void;
  canSchedule?: (block: string) => boolean;
};

const toneOf = (v: unknown): Tone => v === "info" ? "system"
  : v === "success" || v === "warning" || v === "danger" ? v : "neutral";

export function PageRenderer(props: Interactions & { spec: PageSpec; pending?: boolean }) {
  const plan = computed(() => planPage(props.spec, props.data, !!props.pending));
  return <Column gap={3} testID="page-renderer">
    <If when={!plan().supported}><Label color="warning">This page format is not supported by this version of Metro.</Label></If>
    <Each of={plan().blocks} key={b => b.instance}>
      {block => <Block node={block()} interactions={props} />}
    </Each>
    <If when={plan().truncated}><Label color="warning" testID="page-render-limit">This page reached Metro’s display limit.</Label></If>
  </Column>;
}

function Block(props: { node: PlanBlock; interactions: Interactions }) {
  const key = () => props.node.key;
  return <If when={props.node.type !== "Show" || props.node.issue || props.node.children.length || (!!key() && props.interactions.onEdit)}>
    <PageBlockFrame blockKey={key()} changed={!!key() && props.interactions.changed?.has(key()!)}
      onEdit={key() && props.interactions.onEdit ? () => props.interactions.onEdit?.(key()!) : undefined}>
      <If when={props.node.issue} else={<BlockBody node={props.node} interactions={props.interactions} />}>
        {issue => <Label variant="caption" color="muted" testID="page-source-state">{issue()}</Label>}
      </If>
    </PageBlockFrame>
  </If>;
}

function BlockBody(props: { node: PlanBlock; interactions: Interactions }) {
  const n = () => props.node;
  const p = () => n().p;
  const label = () => str(p().label);
  const empty = (fallback = "No data yet.") => str(p().empty) || fallback;
  const tone = () => toneOf(p().tone);
  const children = () => <Each of={n().children} key={b => b.instance}>
    {child => <Block node={child()} interactions={props.interactions} />}
  </Each>;
  const markdown = (text: string) => <PageMarkdown text={text} onLink={props.interactions.readOnly ? undefined : props.interactions.onLink} />;
  switch (n().type) {
    case "Section": return <Section node={props.node}>{children()}</Section>;
    case "Row": return <Row wrap gap={3} align="start">{children()}</Row>;
    case "Grid": {
      const columns = () => Math.floor(Math.max(1, Math.min(4, number(p().columns) ?? 2)));
      return <PageGrid><Each of={n().children} key={b => b.instance}>{child =>
        <PageGridCell columns={columns()}><Block node={child()} interactions={props.interactions} /></PageGridCell>
      }</Each></PageGrid>;
    }
    case "Divider": return <Divider />;
    case "Heading": return <Label heading variant={p().level === 1 ? "title" : p().level === 3 ? "strong" : "heading"}>{n().text}</Label>;
    case "Text": return <Label color={p().muted === true ? "muted" : tone() === "neutral" ? "default" : tone()}>{n().text}</Label>;
    case "Empty": return <Label variant="caption" color="muted">{n().text || empty()}</Label>;
    case "Markdown": return <PageMarkdown text={n().text} onLink={props.interactions.readOnly ? undefined : props.interactions.onLink} />;
    case "Badge": return <Row><Badge tone={tone()} label={n().text} /></Row>;
    case "Stat": return <Surface variant="card" pad={3} gap={1}>
      <Label variant="caption" color="muted">{label()}</Label>
      <Label variant="display" color={cell(p().value, str(p().format)).tone ?? (tone() === "neutral" ? "default" : tone())}>{cell(p().value, str(p().format)).label}</Label>
      <If when={p().hint}><Label variant="caption" color="muted">{str(p().hint)}</Label></If>
    </Surface>;
    case "Progress": {
      const value = () => number(p().value);
      const max = () => number(p().max) ?? 100;
      return <If when={value() !== undefined && max() > 0} else={<Label color="muted">{empty()}</Label>}>
        <PageProgress percent={Math.max(0, Math.min(100, value()! / max() * 100))} label={label() || "Progress"} tone={tone()} />
      </If>;
    }
    case "Chart": return <If when={n().points?.length} else={<Label color="muted">{empty()}</Label>}>
      <PageChart points={n().points ?? []} label={label() || "Chart"} kind={p().kind === "bar" ? "bar" : "line"} />
    </If>;
    case "Table": return <If when={n().rows?.length} else={<Label color="muted">{empty()}</Label>}>
      <PageTable headings={n().headings ?? []} rows={n().rows ?? []} />
    </If>;
    case "List": return <If when={n().children.length} else={<Label color="muted">{empty()}</Label>}>
      <Column gap={1}>{children()}</Column>
    </If>;
    case "Each": case "Show": return <Column gap={2}>{children()}</Column>;
    case "AgentAnswer": {
      const answer = () => n().answer;
      const status = () => cell(own(answer(), "status"), "status");
      return <Column gap={2}>
        <If when={answer()} else={<Label color="muted">{empty("No answer yet.")}</Label>}>
          <Row gap={2} align="center" wrap>
            <Badge tone={status().tone ?? "agent"} label={status().label} />
            <Label variant="meta">{[str(own(answer(), "agent")), str(own(answer(), "room")), cell(own(answer(), "at"), "relative").label].filter(x => x && x !== "—").join(" · ")}</Label>
          </Row>
          <If when={str(own(answer(), "text"))}>{t => markdown(t())}</If>
          <If when={str(own(answer(), "error"))}>{t => <Label color="warning">{t()}</Label>}</If>
        </If>
        <If when={!props.interactions.readOnly && props.interactions.onSchedule && n().key && props.interactions.canSchedule?.(n().key!) ? n().key : undefined}>
          {key => <Row><Action compact variant="ghost" label="Keep this answer fresh…" testID={`page-schedule:${key()}`} onPress={() => props.interactions.onSchedule?.(key())} /></Row>}
        </If>
      </Column>;
    }
    case "RunList": case "MessageFeed": case "ScheduleCard": case "FileList":
      return <If when={n().values?.length} else={<Label color="muted">{empty()}</Label>}>
        <Column gap={2}>
          <Each of={n().values ?? []}>{item => {
            const field = (name: string) => own(item(), name);
            const title = () => str(field(n().type === "RunList" ? "task" : n().type === "MessageFeed" ? "author" : "name"));
            const status = () => cell(field(n().type === "ScheduleCard" ? "last_status" : "status"), "status");
            return <Surface variant="card" pad={2} gap={1}>
              <Row gap={2} wrap align="center">
                <Label variant="strong">{title() || str(field("agent")) || "Untitled"}</Label>
                <If when={status().label !== "—"}><Badge tone={status().tone ?? "neutral"} label={status().label} /></If>
              </Row>
              <If when={n().type === "MessageFeed"} else={
                <Label variant="meta">{[
                  str(field("room")), n().type === "RunList" ? str(field("agent")) : str(field("kind")),
                  cell(field(n().type === "FileList" ? "uploaded" : n().type === "ScheduleCard" ? "next_run" : "started"), "relative").label,
                  n().type === "FileList" ? cell(field("size"), "bytes").label : n().type === "ScheduleCard" && field("enabled") === false ? "Paused" : "",
                ].filter(x => x && x !== "—").join(" · ")}</Label>
              }>
                <PageMarkdown text={str(field("text"))} onLink={props.interactions.readOnly ? undefined : props.interactions.onLink} />
              </If>
              <If when={!props.interactions.readOnly && n().type === "FileList" && props.interactions.onFile && str(field("id"))}>
                <Row><Action compact variant="ghost" label="Open file" onPress={() => props.interactions.onFile?.(str(field("id")))} /></Row>
              </If>
            </Surface>;
          }}</Each>
        </Column>
      </If>;
    case "Button": case "Link": {
      const capability = () => n().key ? capabilityOf(props.interactions.data, n().key!) : undefined;
      const disabled = () => props.interactions.readOnly || !props.interactions.onAction || !n().action || capability()?.enabled !== true;
      const reason = () => props.interactions.readOnly ? "Preview only" : str(capability()?.reason) || "Action unavailable";
      return <Column gap={1} align="start">
        <Action testID={`page-action:${n().key}`} label={label() || "Action"} variant={n().type === "Link" ? "ghost" : tone() === "danger" ? "destructive" : tone() === "system" ? "primary" : "secondary"}
          disabled={disabled() || props.interactions.busy}
          onPress={() => { if (!disabled() && !props.interactions.busy && n().key && n().action) props.interactions.onAction?.(n().key!, n().action!); }} />
        <If when={disabled()}><Label variant="caption" color="muted">{reason()}</Label></If>
      </Column>;
    }
    default: return null;
  }
}

function Section(props: { node: PlanBlock; children?: import("nsh").JSX.Element }) {
  const open = signal(props.node.p.collapsed !== true);
  return <Surface variant="card" pad={3}>
    <Disclosure title={str(props.node.p.title) || "Section"} open={open()} onToggle={open.set}>
      <Column gap={3}>{props.children}</Column>
    </Disclosure>
  </Surface>;
}

import { computed, signal } from "nsh";
import type { ActionKind, PageValues, PageSpec } from "../../api/pages";
import { Action, Badge, Column, Disclosure, Divider, Label, Row, Surface } from "../../ui";
import type { Tone } from "../../ui";
import { PageBlockFrame, PageChart, PageGrid, PageGridCell, PageMarkdown, PageProgress, PageTable } from "../../ui/pageBlocks";
import { capabilityOf, number, planPage, str } from "./renderPlan";
import type { PlanBlock } from "./renderPlan";

type Interactions = {
  data: PageValues | null;
  readOnly?: boolean;
  busy?: boolean;
  waitingForAgent?: boolean;
  changed?: ReadonlySet<string>;
  highlights?: ReadonlyMap<string, "Added" | "Changed">;
  onAction?: (key: string, kind: ActionKind) => void;
  onEdit?: (key: string, label: string) => void;
  onLink?: (url: string) => void;
  onFile?: (id: string) => void;
  onSchedule?: (block: string) => void;
  scheduling?: (block: string) => "available" | "no-prompt" | undefined;
};

const toneOf = (v: unknown): Tone => v === "info" ? "system"
  : v === "success" || v === "warning" || v === "danger" ? v : "neutral";

export function PageRenderer(props: Interactions & { spec: PageSpec; pending?: boolean }) {
  const plan = computed(() => planPage(props.spec, props.data, !!props.pending));
  return <Column gap={4} testID="page-renderer">
    <If when={!plan().supported}><Label color="warning">This page format is not supported by this version of Metro.</Label></If>
    <If when={plan().richText.limited}>
      <Label color="warning" testID="page-rich-text-limit">Some rich text was shortened because this page reached its display limit.</Label>
    </If>
    <Each of={plan().blocks} key={b => b.instance}>
      {block => <Block node={block()} interactions={props} />}
    </Each>
    <If when={plan().truncated}><Label color="warning" testID="page-render-limit">This page reached Metro’s display limit.</Label></If>
  </Column>;
}

function Block(props: { node: PlanBlock; interactions: Interactions }) {
  const key = () => props.node.key;
  const name = () => blockLabel(props.node);
  return <If when={props.node.type !== "Show" || props.node.issue || props.node.children.length || (!!key() && props.interactions.onEdit)}>
    <PageBlockFrame blockKey={key()} blockLabel={name()} highlight={key()
      ? props.interactions.highlights?.get(key()!) ?? (props.interactions.changed?.has(key()!) ? "Changed" : undefined) : undefined}
      onEdit={key() && props.interactions.onEdit ? () => props.interactions.onEdit?.(key()!, name()) : undefined}>
      <If when={props.node.issue !== undefined} else={<BlockBody node={props.node} interactions={props.interactions} />}>
        <Label variant="caption" color="muted" testID="page-source-state">{props.node.issue}</Label>
      </If>
    </PageBlockFrame>
  </If>;
}

/** A List's children grouped by the item they were rendered for (`<list>#<index>/…`). */
export function listItems(node: PlanBlock) {
  const items: { id: string; blocks: PlanBlock[] }[] = [];
  for (const child of node.children) {
    const rest = child.instance.startsWith(`${node.instance}#`) ? child.instance.slice(node.instance.length + 1) : child.instance;
    const slash = rest.indexOf("/");
    const id = slash < 0 ? rest : rest.slice(0, slash);
    const last = items[items.length - 1];
    if (last?.id === id) last.blocks.push(child);
    else items.push({ id, blocks: [child] });
  }
  return items;
}

function blockLabel(node: PlanBlock) {
  const type = node.type.replace(/([a-z])([A-Z])/g, "$1 $2").toLowerCase();
  const title = str(node.p.title) || str(node.p.label) || (node.type === "Heading" ? node.text : "");
  return title ? `${type}: ${title.slice(0, 120)}` : `${type} block`;
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
    case "Heading": return <Label heading variant={p().level === 1 ? "display" : p().level === 3 ? "heading" : "title"}>{n().text}</Label>;
    case "Text": return <Label color={p().muted === true ? "muted" : tone() === "neutral" ? "default" : tone()}>{n().text}</Label>;
    case "Empty": return <Label variant="caption" color="muted">{n().text || empty()}</Label>;
    case "Markdown": return <If when={n().document}>{document =>
      <PageMarkdown document={document()} onLink={props.interactions.readOnly ? undefined : props.interactions.onLink} />
    }</If>;
    case "Badge": return <Row><Badge tone={tone()} label={n().text} /></Row>;
    case "Stat": return <Surface variant="card" pad={3} gap={1}>
      <Label variant="caption" color="muted">{label()}</Label>
      <Label variant="display" tabular color={n().stat?.tone ?? (tone() === "neutral" ? "default" : tone())}>{n().stat?.label}</Label>
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
      <PageTable label={label()} headings={n().headings ?? []} rows={n().rows ?? []} numericColumns={n().numericColumns} />
    </If>;
    case "List": return <If when={n().children.length} else={<Label color="muted">{empty()}</Label>}>
      <Column testID="page-list">
        <Each of={listItems(n())} key={item => item.id}>{(item, index) => <Column gap={1}>
          <If when={index() > 0}><Divider /></If>
          <Column padY={2} gap={1} testID="page-list-item">
            <Each of={item().blocks} key={b => b.instance}>{child => <Block node={child()} interactions={props.interactions} />}</Each>
          </Column>
        </Column>}</Each>
      </Column>
    </If>;
    case "Each": case "Show": return <Column gap={2}>{children()}</Column>;
    case "AgentAnswer": {
      const answer = () => n().answer;
      const scheduling = () => !props.interactions.readOnly && n().key ? props.interactions.scheduling?.(n().key!) : undefined;
      return <Surface variant="card" pad={3} gap={2}>
        <If when={scheduling()}><Label variant="meta">Latest room answer</Label></If>
        <If when={answer()} else={<Label color="muted">{empty("No answer yet.")}</Label>}>{item => <>
          <Row gap={2} align="center" wrap>
            <Badge tone={item().status.tone ?? "agent"} label={item().status.label} />
            <Label variant="meta">{item().metadata}</Label>
          </Row>
          <If when={n().document}>
            {document => <PageMarkdown document={document()} onLink={props.interactions.readOnly ? undefined : props.interactions.onLink} />}
          </If>
          <If when={item().error}>{t => <Label color="warning">{t()}</Label>}</If>
        </>}</If>
        <If when={scheduling() === "available"}><Row><Action compact variant="ghost" label="Schedule brief…"
          accessibilityLabel="Schedule a brief for this room answer" testID={`page-schedule:${n().key}`}
          disabled={props.interactions.busy} onPress={() => { if (n().key) props.interactions.onSchedule?.(n().key!); }} /></Row></If>
        <If when={scheduling() === "no-prompt"}><Label variant="caption" color="muted">Scheduling needs one matching agent button with a prompt.</Label></If>
      </Surface>;
    }
    case "RunList": case "MessageFeed": case "ScheduleCard": case "FileList":
      return <If when={n().entries?.length} else={<Label color="muted">{empty()}</Label>}>
        <Column>
          <Each of={n().entries ?? []}>{(item, index) => {
            const status = () => item().status;
            return <Column gap={1}>
              <If when={index() > 0}><Divider /></If>
              <Row padY={2} gap={3} align="center">
                <Column grow gap={1}>
                  <Label variant="strong">{item().title || "Untitled"}</Label>
                  <If when={n().type === "MessageFeed"} else={<Label variant="meta">{item().metadata}</Label>}>
                    <If when={n().messages?.[index()]}>{document =>
                      <PageMarkdown document={document()} onLink={props.interactions.readOnly ? undefined : props.interactions.onLink} />
                    }</If>
                  </If>
                </Column>
                <If when={status().label && status().label !== "—"}><Badge tone={status().tone ?? "neutral"} label={status().label} /></If>
                <If when={!props.interactions.readOnly && props.interactions.onFile ? item().fileId : undefined}>{id =>
                  <Action compact variant="ghost" label="Open file" accessibilityLabel={`Open file: ${item().title || "Untitled"}`} onPress={() => props.interactions.onFile?.(id())} />
                }</If>
              </Row>
            </Column>;
          }}</Each>
        </Column>
      </If>;
    case "Button": case "Link": {
      const capability = () => n().key ? capabilityOf(props.interactions.data, n().key!) : undefined;
      const disabled = () => props.interactions.readOnly || !props.interactions.onAction || !n().action || capability()?.enabled !== true;
      const reason = () => props.interactions.readOnly ? "Preview only" : n().disabledReason || "Action unavailable";
      return <Column gap={1} align="start">
        <Action testID={`page-action:${n().key}`} label={label() || "Action"} variant={n().type === "Link" ? "ghost" : tone() === "danger" ? "destructive" : tone() === "system" ? "primary" : "secondary"}
          disabled={disabled() || props.interactions.busy || (n().action === "runAgent" && props.interactions.waitingForAgent)}
          onPress={() => { if (!disabled() && !props.interactions.busy && n().key && n().action) props.interactions.onAction?.(n().key!, n().action!); }} />
        <If when={disabled()}><Label variant="caption" color="muted">{reason()}</Label></If>
      </Column>;
    }
    default: return null;
  }
}

function Section(props: { node: PlanBlock; children?: import("nsh").JSX.Element }) {
  const open = signal(props.node.p.collapsed !== true);
  return <Disclosure lined title={str(props.node.p.title) || "Section"} open={open()} onToggle={open.set}>
    <Column gap={3}>{props.children}</Column>
  </Disclosure>;
}

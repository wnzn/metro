import { Clipboard, announce, signal } from "nsh";
import type { RoomEvent } from "../../api/client";
import { Action, Column, Disclosure, Label, MessageBody, Row } from "../../ui";
import type { Stamp } from "../chatTime";

export function CompactionMarker(props: { event: RoomEvent; stamp: Stamp | null; scope: string }) {
  const summary = () => props.event.compaction!;
  const open = signal(false);
  const id = () => `compaction:${props.event.id}`;
  const title = () => `Earlier messages summarized · ${summary().events} messages`;
  const attribution = () => [
    `Summarized for ${summary().byName}`,
    props.stamp?.full,
    summary().focus ? `focus: ${summary().focus}` : null,
  ].filter(Boolean).join(" · ");
  const copy = () => void Clipboard.writeText(props.event.text).then(
    () => announce("Summary copied"), () => announce("Could not copy the summary"),
  );
  return (
    <Column padX={3} padY={2}>
      <Disclosure centered title={title()} open={open()} onToggle={open.set} testID={id()}
        accessibilityLabel={`${title()}, ${open() ? "collapse summary" : "expand to read the summary"}`}>
        <MessageBody text={props.event.text} scopeKey={props.scope} testID={`${id()}:body`} />
        <Label variant="caption" color="muted">{attribution()}</Label>
        <If when={summary().trigger === "auto"}><Label variant="caption" color="muted">Automatic</Label></If>
        <Row><Action compact variant="ghost" label="Copy summary" onPress={copy} testID={`${id()}:copy`} /></Row>
      </Disclosure>
    </Column>
  );
}

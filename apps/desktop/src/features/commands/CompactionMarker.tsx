import { Clipboard, announce, signal } from "nsh";
import type { RoomEvent } from "../../api/client";
import { Action, Column, Disclosure, Label, MessageBody, Row, Timestamp } from "../../ui";
import type { Stamp } from "../chatTime";

export function CompactionMarker(props: { event: RoomEvent; stamp: Stamp | null; scope: string }) {
  const summary = () => props.event.compaction!;
  const open = signal(false);
  const id = () => `compaction:${props.event.id}`;
  const title = () => `Earlier messages summarized · ${summary().events} messages`;
  const copy = () => void Clipboard.writeText(props.event.text).then(
    () => announce("Summary copied"), () => announce("Could not copy the summary"),
  );
  return (
    <Column padX={3} padY={2}>
      <Disclosure centered title={title()} open={open()} onToggle={open.set} testID={id()}
        accessibilityLabel={`${title()}, ${open() ? "collapse summary" : "expand to read the summary"}`}>
        <MessageBody text={props.event.text} scopeKey={props.scope} testID={`${id()}:body`} />
        <Row gap={1} align="center" wrap>
          <Label variant="caption" color="muted">Summarized for {summary().byName}</Label>
          <If when={props.stamp}>
            {s => <Timestamp text={`· ${s().short}`} full={s().full} testID={`${id()}:time`} />}
          </If>
          <If when={summary().focus}>
            {focus => <Label variant="caption" color="muted">· focus: {focus()}</Label>}
          </If>
        </Row>
        <If when={summary().trigger === "auto"}><Label variant="caption" color="muted">Automatic</Label></If>
        <Row><Action compact variant="ghost" label="Copy summary" onPress={copy} testID={`${id()}:copy`} /></Row>
      </Disclosure>
    </Column>
  );
}

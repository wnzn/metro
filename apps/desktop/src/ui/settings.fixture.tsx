// Standalone UI fixture: no Settings features, server or desktop bridge.
import { mount, signal } from "nsh";
import {
  Action, Badge, Checkbox, Column, ConfirmInline, ConnectionItem, Field, Label, Modal, ModalNav, ModalNavSection,
  ModalTab, ModalTabRow, PageHeader, ProblemNote, ProgressLine, RailButton, RailDivider, ReorderButtons, Row,
  ScrollArea, Surface, compactWidth,
} from "./index";
import type { PageScope } from "./index";

const scopes: PageScope[] = [
  { kind: "account", host: "metro.example" },
  { kind: "computer" },
  { kind: "space" },
  { kind: "space", personal: true },
  { kind: "organization" },
];

function Fixture() {
  const visible = signal(true);
  const selected = signal("profile");
  const update = signal(true);
  const scope = signal(0);
  const checked = signal(true);
  const beforeCheckbox = signal("");
  const disabled = signal(false);
  const target = signal("A");
  const confirmed = signal(0);
  const open = signal(false);
  const busy = signal(false);
  const position = signal(0);
  const rowAction = signal("none");
  const progress = signal<number | undefined>(42);
  const notice = () => update() ? "Update available" : undefined;
  const tabs = (compact: boolean) => (
    <>
      <ModalNavSection label="You" testID="fx-section-you" compact={compact} />
      <ModalTab label="Profile" icon="person" testID="fx-tab-profile" selected={selected() === "profile"} compact={compact} onPress={() => selected.set("profile")} />
      <ModalNavSection label="Metro" testID="fx-section-metro" compact={compact} divided />
      <ModalTab label="About & updates" icon="metro" testID="fx-tab-about" selected={selected() === "about"} compact={compact}
        dot={notice()} onPress={() => selected.set("about")} />
    </>
  );
  const page = (
    <ScrollArea pad={4} gap={3} testID="fx-scroll">
      <PageHeader title={selected() === "about" ? "About" : "Profile page"} subtitle="A fixture subtitle." scope={scopes[scope()]} testID="fx-header">
        <Action label="Next scope" onPress={() => scope.set((scope() + 1) % scopes.length)} testID="fx-next-scope" />
      </PageHeader>
      <Row gap={2} wrap>
        <Action label="Toggle update" onPress={() => update.set(!update())} testID="fx-update" />
        <Action label="Toggle disabled" onPress={() => disabled.set(!disabled())} testID="fx-disabled" />
        <Action label="Switch target" onPress={() => target.set(target() === "A" ? "B" : "A")} testID="fx-retarget" />
      </Row>
      <Field value={beforeCheckbox()} onChange={beforeCheckbox.set} accessibilityLabel="Field before checkbox" testID="fx-before-checkbox" />
      <Checkbox label="Use the runner" hint="Only uses this computer." checked={checked()} disabled={disabled()} onChange={checked.set} testID="fx-checkbox" />
      <Label>{`Runner selected: ${checked()}`}</Label>
      <ConfirmInline triggerLabel="Delete…" confirmLabel="Delete Space" message="This deletes the Space for everyone."
        typeToConfirm="Exact Name" scopeKey={target()} onConfirm={() => confirmed.set(confirmed() + 1)} testID="fx-confirm" />
      <Label>{`Confirmations: ${confirmed()}`}</Label>
      <Action label="Open update confirmation" testID="fx-external-open" onPress={() => open.set(true)} />
      <ConfirmInline open={open()} onOpenChange={open.set} variant="primary" confirmLabel="Download and verify" message="The app will restart."
        onConfirm={() => busy.set(true)} busy={busy()} busyLabel="Downloading…" testID="fx-external" />
      <ConnectionItem name="Test provider" mark={{ letters: "TP", tone: "neutral" }} isDefault status={{ tone: "success", text: "Online" }}
        tags={<Badge tone="info" label="Update 0.5.0" testID="fx-update-tag" />} tagsLabel="Update 0.5.0"
        onPress={() => rowAction.set("open")} trailing={<Action label="Details" testID="fx-row-action" onPress={() => rowAction.set("trailing")} />}
        testID="fx-row">
        <ProblemNote text="A problem below the tagged row." testID="fx-problem" />
      </ConnectionItem>
      <ConnectionItem name="Passive provider" leading={<Label>PP</Label>} status={{ tone: "neutral", text: "Off" }} testID="fx-passive">
        <Label>{`Last row action: ${rowAction()}`}</Label>
      </ConnectionItem>
      <ReorderButtons label="Test provider" canMoveUp={position() > 0} canMoveDown={position() < 1} disabled={disabled()}
        onMove={(step) => position.set(position() + step)} testID="fx-reorder" />
      <Label>{`Provider position: ${position() + 1}`}</Label>
      <ProgressLine label="Downloading 0.5.0…" value={progress()} testID="fx-progress" />
      <Action label="Advance progress" testID="fx-advance" onPress={() => progress.set(progress() === 42 ? 150 : progress() === 150 ? undefined : -10)} />
    </ScrollArea>
  );
  return (
    <Surface variant="app">
      <Modal title="Fixture settings" testID="fx-modal" open={visible()} onClose={() => visible.set(false)}>
        <If when={compactWidth()} else={
          <Row grow>
            <ModalNav testID="fx-nav" accessibilityLabel="Fixture sections">{tabs(false)}</ModalNav>
            <Surface variant="rail"><RailDivider /><RailButton label="Settings" icon="settings" dot={notice()} dotTestID="fx-rail-dot" onPress={() => selected.set("about")} /></Surface>
            {page}
          </Row>
        }>
          <Column grow>
            <ModalTabRow testID="fx-nav" accessibilityLabel="Fixture sections">{tabs(true)}</ModalTabRow>
            {page}
          </Column>
        </If>
      </Modal>
    </Surface>
  );
}

mount(Fixture);

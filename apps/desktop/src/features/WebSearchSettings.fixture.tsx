import { mount, signal } from "nsh";
import { BASE, TOKENS } from "../../test/fakeServer";
import { connect, status } from "../store";
import { Action, Column, Label, Row } from "../ui";
import { WebSearchSettings } from "./WebSearchSettings";

function Fixture() {
  const show = signal(true);
  const link = signal("");
  const use = (server: string, account: string) => void connect(server, TOKENS[account]!, { remember: false });
  return (
    <Column grow>
      <Row gap={2}>
        <Action label="Alice" testID="fixture-alice" onPress={() => use(BASE, "alice")} />
        <Action label="Bob" testID="fixture-bob" onPress={() => use(BASE, "bob")} />
        <Action label="Other workspace" testID="fixture-other" onPress={() => use(BASE.replace("8787", "8788"), "alice")} />
        <Action label="Toggle page" testID="fixture-toggle" onPress={() => show.set(!show())} />
      </Row>
      <Label testID="fixture-link">{link()}</Label>
      <If when={show() && status().phase === "online"}>
        <WebSearchSettings onOpenAbout={() => link.set("About opened")} onOpenPrivacy={() => link.set("Privacy opened")} />
      </If>
    </Column>
  );
}
mount(Fixture);

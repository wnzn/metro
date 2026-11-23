import { mount } from "nsh";
import { BASE, TOKENS } from "../../../../test/fakeServer";
import { connect, openChat, selectSpace, status } from "../../../store";
import { Action, Column } from "../../../ui";
import { ComposerModelControls } from "../index";

function Harness() {
  void connect(BASE, TOKENS.alice!, { remember: false });
  return <If when={status().phase === "online"}>
    <Column>
      <Action label="Acme" onPress={() => void selectSpace("acme")} />
      <Action label="Personal" onPress={openChat} />
      <ComposerModelControls />
    </Column>
  </If>;
}
mount(Harness);

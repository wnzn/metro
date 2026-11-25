import { mount, signal } from "nsh";
import { Action, Column, Label, PageHeader, ScrollArea, Surface } from "../../../ui";
import { PageRenderer } from "../PageRenderer";
import { sample, sampleData } from "./sample";

/** Native renderer fixture: no clients, tokens, process helpers or network. */
function App() {
  const review = signal(false);
  const selected = signal("");
  return <Surface variant="app">
    <Column grow pad={4} gap={2}>
      <PageHeader title="Pages fixture (local)">
        <Action testID="fixture-draft" label={review() ? "Show live" : "Show draft"} onPress={() => review.set(!review())} />
      </PageHeader>
      <Label>{selected()}</Label>
      <ScrollArea gap={2}>
        <PageRenderer spec={sample} data={sampleData} readOnly={review()} changed={review() ? new Set(["stat", "answer"]) : undefined}
          onAction={(key, kind) => selected.set(`${kind}: ${key}`)} />
      </ScrollArea>
    </Column>
  </Surface>;
}

mount(App);

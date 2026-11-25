import { mount, signal } from "nsh";
import { Action, Column, Label, PageHeader, ScrollArea, Surface } from "../../../ui";
import { PageRenderer } from "../PageRenderer";
import { bind, lit, plainBudgetData, plainBudgetSpec, sample, sampleData } from "./sample";

const wideTable = { ...sample, blocks: [{ type: "Table", key: "wide-table", props: { label: lit("Run details"), rows: bind("runs", "items") },
  children: Array.from({ length: 10 }, (_, i) => ({ type: "Column", props: { field: lit("agent"), label: lit(`Column ${i + 1}`) } })) }] };

/** Native renderer fixture: no clients, tokens, process helpers or network. */
function App() {
  const review = signal(false);
  const selected = signal("");
  const budget = signal(false);
  const wide = signal(false);
  return <Surface variant="app">
    <Column grow pad={4} gap={2}>
      <PageHeader title="Pages fixture (local)">
        <Action testID="fixture-draft" label={review() ? "Show live" : "Show draft"} onPress={() => review.set(!review())} />
        <Action testID="fixture-budget" label="Show text limit fixture" onPress={() => budget.set(true)} />
        <Action testID="fixture-wide-table" label="Show wide table" onPress={() => wide.set(true)} />
      </PageHeader>
      <Label>{selected()}</Label>
      <ScrollArea gap={2}>
        <PageRenderer spec={budget() ? plainBudgetSpec : wide() ? wideTable : sample} data={budget() ? plainBudgetData : sampleData} readOnly={review()} changed={review() ? new Set(["stat", "answer"]) : undefined}
          onAction={(key, kind) => selected.set(`${kind}: ${key}`)} />
      </ScrollArea>
    </Column>
  </Surface>;
}

mount(App);

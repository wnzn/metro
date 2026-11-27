import { Text, View, batch, keymap, mount, signal } from "nsh";
import { Button } from "nsh/base";
import { connectedTo, me } from "../store";
import "./appearance";
import { Modal } from "./chrome";
import { ModelPicker } from "./modelPicker";
import type { ModelOption } from "./modelPicker";
import type { PickerKind } from "./pickerMemory";

const DEFAULT: ModelOption = { value: "", model: "Default model", title: "Default model — normal routing" };
const OPENAI = { key: "openai", name: "OpenAI", mark: { letters: "O", tone: "agent", logo: "openai" } } as const;
const ANTHROPIC = { key: "anthropic", name: "Anthropic", mark: { letters: "A", tone: "warning" } } as const;
const OPTIONS: readonly ModelOption[] = [
  DEFAULT,
  { value: "alpha", model: "Alpha", modelId: "alpha-1", title: "Alpha · Work connection", provider: OPENAI },
  { value: "beta", model: "Beta", title: "Beta · not ready", provider: OPENAI, disabled: true },
  { value: "claude", model: "Claude", title: "Claude", provider: ANTHROPIC },
];

const SERVER = "https://picker.example";
const USER = "person:1";

function Fixture() {
  const open = signal(false);
  const value = signal("");
  const calls = signal(0);
  const preceding = signal(0);
  const trailing = signal(0);
  const parentFocus = signal<"previous" | "next" | null>(null);
  const context = signal("one");
  const kind = signal<PickerKind>("composer");
  const renamed = signal(false);
  const single = signal(false);
  const disabled = signal(false);
  const long = signal(false);
  batch(() => { connectedTo.set(SERVER); me.set(USER); });
  keymap.bind({
    "mod+1": () => me.set("person:2"),
    "mod+2": () => batch(() => { me.set(USER); connectedTo.set("https://another.example"); }),
    "mod+3": () => batch(() => { connectedTo.set(SERVER); me.set(USER); kind.set("routing"); }),
    "mod+4": () => kind.set("composer"),
    "mod+5": () => context.set("two"),
    "mod+6": () => renamed.set(true),
    "mod+7": () => single.set(true),
    "mod+8": () => disabled.set(!disabled()),
    "mod+0": () => long.set(true),
  }, { when: "always" });
  const extra = Array.from({ length: 40 }, (_, i) => ({
    value: `extra-${i}`, model: `Additional model ${i}`, title: `Additional model ${i}`, provider: OPENAI,
  }));
  const options = () => [...OPTIONS, ...(long() ? extra : [])]
    .filter((o) => !single() || o.provider?.key !== "anthropic")
    .map((o) => renamed() && o.provider?.key === "openai"
      ? { ...o, provider: { ...o.provider, name: "OpenAI renamed" } }
      : o);
  const blur = (target: "previous" | "next") => {
    if (parentFocus() === target) parentFocus.set(null);
  };

  return (
    <View class="flex-1 bg-background">
      <Button onPress={() => open.set(true)} accessibilityLabel="Open fixture" testID="open">
        <Text class="text-foreground">Open fixture</Text>
      </Button>
      <Modal title="Picker fixture" size="compact" open={open()} onClose={() => open.set(false)}>
        <View class="gap-(--space-2)">
          <View role="button" testID="outside" onPress={() => preceding.set(preceding() + 1)}
            onFocus={() => parentFocus.set("previous")} onBlur={() => blur("previous")}>
            <Text class="text-foreground">{`Previous control: ${preceding()}`}</Text>
          </View>
          <ModelPicker
            options={options()}
            value={value()}
            onChange={(next) => batch(() => { value.set(next); calls.set(calls() + 1); })}
            rememberAs={kind()}
            context={context()}
            disabled={disabled()}
            testID="picker"
          />
          <View role="button" testID="next" onPress={() => trailing.set(trailing() + 1)}
            onFocus={() => parentFocus.set("next")} onBlur={() => blur("next")}>
            <Text class="text-foreground">{`Next control: ${trailing()}`}</Text>
          </View>
          <Text class="text-foreground">{`Parent focus: ${parentFocus() ?? "neither"}`}</Text>
          <Text class="text-foreground">{`Choices: ${calls()}`}</Text>
        </View>
      </Modal>
    </View>
  );
}

mount(Fixture);

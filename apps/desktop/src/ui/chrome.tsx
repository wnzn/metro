// App chrome primitives: icons, the modal frame, brand marks, the provider
// picker and the search box. Like ./primitives, feature code passes only
// semantic props; every class and color decision lives here or in
// theme/theme.json.
//
// Icons are one family drawn for Metro (./icons, 24-unit grid, 1.8 stroke,
// round caps), monochrome templates tinted with theme tokens, so they follow
// light and dark. Provider marks are bundled monochrome SVGs from
// @lobehub/icons-static-svg 1.105.0 (MIT; ./icons/LICENSE-brand-marks.txt),
// tinted with the theme's foreground, shown only beside the provider's own
// name; any other provider gets a lettermark. Nothing is fetched at runtime.

import { Image, Text, View, token, windowSize } from "nsh";
import type { ImageAsset, JSX } from "nsh";
import { Button, Dialog, Input, Select } from "nsh/base";
import appearanceIcon from "./icons/appearance.svg";
import chatIcon from "./icons/chat.svg";
import checkIcon from "./icons/check.svg";
import chevronDownIcon from "./icons/chevron-down.svg";
import chevronRightIcon from "./icons/chevron-right.svg";
import clockIcon from "./icons/clock.svg";
import closeIcon from "./icons/close.svg";
import copyIcon from "./icons/copy.svg";
import deviceIcon from "./icons/device.svg";
import hashIcon from "./icons/hash.svg";
import logoutIcon from "./icons/logout.svg";
import personIcon from "./icons/person.svg";
import plusIcon from "./icons/plus.svg";
import providersIcon from "./icons/providers.svg";
import routingIcon from "./icons/routing.svg";
import runnerIcon from "./icons/runner.svg";
import searchIcon from "./icons/search.svg";
import serverIcon from "./icons/server.svg";
import settingsIcon from "./icons/settings.svg";
import threadIcon from "./icons/thread.svg";
import brandOpenai from "./icons/brand-openai.svg";
import brandAnthropic from "./icons/brand-anthropic.svg";
import brandGemini from "./icons/brand-gemini.svg";
import brandEmbeddinggemma from "./icons/brand-embeddinggemma.svg";
import brandAzureOpenai from "./icons/brand-azure-openai.svg";
import brandDeepseek from "./icons/brand-deepseek.svg";
import brandGroq from "./icons/brand-groq.svg";
import brandTogether from "./icons/brand-together.svg";
import brandHuggingface from "./icons/brand-huggingface.svg";
import brandMistral from "./icons/brand-mistral.svg";
import brandOpenrouter from "./icons/brand-openrouter.svg";
import brandVenice from "./icons/brand-venice.svg";
import brandZai from "./icons/brand-zai.svg";
import brandMinimax from "./icons/brand-minimax.svg";
import brandMoonshot from "./icons/brand-moonshot.svg";
import brandXiaomimimo from "./icons/brand-xiaomimimo.svg";
import brandCohere from "./icons/brand-cohere.svg";
import brandOllama from "./icons/brand-ollama.svg";
import brandOpencode from "./icons/brand-opencode.svg";
import brandXai from "./icons/brand-xai.svg";
import brandSystemone from "./icons/brand-systemone.svg";
import { cx, toneSurfaceClass, toneTextClass } from "./tokens";
import type { Tone } from "./tokens";

type Testable = { testID?: string; accessibilityLabel?: string };

// ---------------------------------------------------------------- icons

const ICONS = {
  appearance: appearanceIcon,
  chat: chatIcon,
  check: checkIcon,
  "chevron-down": chevronDownIcon,
  "chevron-right": chevronRightIcon,
  clock: clockIcon,
  close: closeIcon,
  copy: copyIcon,
  device: deviceIcon,
  hash: hashIcon,
  logout: logoutIcon,
  person: personIcon,
  plus: plusIcon,
  providers: providersIcon,
  routing: routingIcon,
  runner: runnerIcon,
  search: searchIcon,
  server: serverIcon,
  settings: settingsIcon,
  thread: threadIcon,
} as const;

export type IconName = keyof typeof ICONS;
export type IconColor = "default" | "muted" | "sidebar" | "sidebar-active" | "accent";

const ICON_TOKEN = {
  default: "foreground",
  muted: "muted-foreground",
  sidebar: "sidebar-foreground",
  "sidebar-active": "sidebar-accent-foreground",
  accent: "accent-foreground",
} as const;

/** A decorative icon: screen readers skip it; say what it means in the control's label. */
export function Icon(props: { name: IconName; color?: IconColor; size?: 14 | 16 | 20 }) {
  const size = () => props.size ?? 16;
  return (
    <Image
      src={ICONS[props.name]}
      fadeDuration={0}
      style={{ width: size(), height: size(), tintColor: token(ICON_TOKEN[props.color ?? "muted"]) }}
      accessibilityElementsHidden
    />
  );
}

const focusRing = "focus-visible:ring-2 focus-visible:ring-ring";

/** A square button with only an icon. `accessibilityLabel` is required: it is all a screen reader hears. */
export function IconButton(props: { icon: IconName; accessibilityLabel: string; onPress: () => void; testID?: string; disabled?: boolean; tooltip?: string }) {
  return (
    <Button
      class={cx("items-center justify-center size-[30px] rounded-(--radius-sm) hover:bg-muted active:bg-muted disabled:opacity-50", focusRing)}
      onPress={props.onPress}
      disabled={props.disabled}
      accessibilityLabel={props.accessibilityLabel}
      testID={props.testID}
      tooltip={props.tooltip ? <Text class="rounded-(--radius-sm) bg-popover border border-border px-(--space-2) py-[3px] text-(length:--text-xs) text-popover-foreground">{props.tooltip}</Text> : undefined}
    >
      <Icon name={props.icon} color="muted" />
    </Button>
  );
}

// ---------------------------------------------------------------- responsive

/** The window is narrow: panels stack, side navigation turns into tabs. */
export const compactWidth = () => windowSize().width < 900;

// ---------------------------------------------------------------- modal

/**
 * A large modal over the app, in its own native layer: it takes the keyboard
 * focus and gives it back as it closes; Escape, the backdrop and `onClose`'s
 * callers close it. Sized to the window, never past it.
 */
export function Modal(props: Testable & { open: boolean; title: string; onClose: () => void; children?: JSX.Element }) {
  const size = () => {
    const w = windowSize();
    return { width: Math.max(320, Math.min(1080, w.width - 48)), height: Math.max(360, Math.min(800, w.height - 48)) };
  };
  return (
    <Dialog
      open={props.open}
      animation="fade"
      accessibilityLabel={props.title}
      testID={props.testID}
      backdropTestID={props.testID ? `${props.testID}-backdrop` : undefined}
      onOpenChange={(e) => {
        if (!e.open) props.onClose();
      }}
      backdropClass="bg-black/55"
      class="m-auto flex-col overflow-hidden rounded-(--radius-lg) border border-border bg-background shadow-lg"
      style={size()}
    >
      {props.children}
    </Dialog>
  );
}

/** The side navigation of a modal (Settings' sections). */
export function ModalNav(props: Testable & { children?: JSX.Element }) {
  return (
    <View class="w-[228px] flex-col gap-[2px] bg-sidebar border-r border-sidebar-border px-(--space-2) py-(--space-3)" testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="tablist">
      {props.children}
    </View>
  );
}

/** One section of a modal's navigation: a tab, side by side or (compact) in a row. */
export function ModalTab(props: { label: string; icon: IconName; selected: boolean; onPress: () => void; testID?: string; compact?: boolean }) {
  return (
    <Button
      class={cx(
        "flex-row items-center gap-(--space-2) rounded-(--radius-sm) px-(--space-2) py-[7px] hover:bg-sidebar-accent/60 active:bg-sidebar-accent",
        focusRing,
        props.compact && "px-(--space-3)",
        props.selected && "bg-sidebar-accent",
      )}
      role="tab"
      selected={props.selected}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.label}
    >
      <Icon name={props.icon} color={props.selected ? "sidebar-active" : "sidebar"} />
      <Text class={cx("text-(length:--text-sm)", props.selected ? "font-semibold text-sidebar-accent-foreground" : "text-sidebar-foreground")} style={{ numberOfLines: 1 }}>
        {props.label}
      </Text>
    </Button>
  );
}

/** A modal's tabs in one wrapping row (compact windows). */
export function ModalTabRow(props: Testable & { children?: JSX.Element }) {
  return (
    <View class="flex-row flex-wrap gap-[2px] bg-sidebar border-b border-sidebar-border px-(--space-2) py-(--space-2)" testID={props.testID} role="tablist">
      {props.children}
    </View>
  );
}

// ---------------------------------------------------------------- brand marks

/** Bundled provider marks, by Metro's provider key (see ./icons/LICENSE-brand-marks.txt). */
const BRAND_LOGOS: Record<string, ImageAsset> = {
  "openai": brandOpenai,
  "anthropic": brandAnthropic,
  "gemini": brandGemini,
  "embeddinggemma": brandEmbeddinggemma,
  "azure-openai": brandAzureOpenai,
  "deepseek": brandDeepseek,
  "groq": brandGroq,
  "together": brandTogether,
  "huggingface": brandHuggingface,
  "mistral": brandMistral,
  "openrouter": brandOpenrouter,
  "venice": brandVenice,
  "zai": brandZai,
  "minimax": brandMinimax,
  "moonshot": brandMoonshot,
  "xiaomimimo": brandXiaomimimo,
  "cohere": brandCohere,
  "ollama": brandOllama,
  "opencode": brandOpencode,
  "xai": brandXai,
  "systemone": brandSystemone,
};

/** Whether a bundled mark exists for this provider key. */
export const hasBrandLogo = (key: string) => Object.prototype.hasOwnProperty.call(BRAND_LOGOS, key);

/**
 * How a provider is drawn: its bundled mark (`logo`, a key of the bundled
 * marks) when there is one, else a short lettermark in one of the theme's tones.
 */
export type Mark = { letters: string; tone: Tone; logo?: string };

/** A provider's mark tile. Decorative: the name next to it is what screen readers hear. */
export function BrandMark(props: { mark: Mark; size?: "sm" | "md" }) {
  const logo = () => (props.mark.logo && hasBrandLogo(props.mark.logo) ? props.mark.logo : null);
  const md = () => props.size === "md";
  return (
    <If
      when={logo()}
      else={
        <View
          class={cx("items-center justify-center rounded-(--radius-sm)", md() ? "size-[30px]" : "size-[22px]", toneSurfaceClass[props.mark.tone])}
          accessibilityElementsHidden
          testID={`mark-letters:${props.mark.letters}`}
        >
          <Text class={cx("font-semibold", md() ? "text-(length:--text-sm)" : "text-[10px]", toneTextClass[props.mark.tone])}>{props.mark.letters}</Text>
        </View>
      }
    >
      {(key) => (
        <View
          class={cx("items-center justify-center rounded-(--radius-sm) border border-border bg-background", md() ? "size-[30px]" : "size-[22px]")}
          accessibilityElementsHidden
          testID={`mark-logo:${key()}`}
        >
          <Image
            src={BRAND_LOGOS[key()]}
            fadeDuration={0}
            style={{ width: md() ? 18 : 14, height: md() ? 18 : 14, tintColor: token("foreground") }}
            accessibilityElementsHidden
          />
        </View>
      )}
    </If>
  );
}

export type BrandItem = { value: string; label: string; hint?: string; mark: Mark; disabled?: boolean };

/**
 * A compact drop-down of brands (model providers): each row a lettermark, a
 * name and a short hint. Searchable; the trigger shows the chosen one.
 */
export function BrandPicker(props: {
  accessibilityLabel: string;
  items: readonly BrandItem[];
  value: string | null;
  onChange: (value: string | null) => void;
  placeholder: string;
  testID?: string;
}) {
  const current = () => props.items.find((item) => item.value === props.value) ?? null;
  const byValue = () => new Map(props.items.map((item) => [item.value, item]));
  return (
    <Select
      class={cx(
        "flex-row items-center gap-(--space-2) min-h-[40px] w-full max-w-[420px] rounded-(--radius-sm) border border-input bg-background px-(--space-2) hover:bg-muted",
        focusRing,
      )}
      items={props.items.map((item) => ({ value: item.value, title: item.hint ? `${item.label} — ${item.hint}` : item.label, disabled: item.disabled }))}
      value={props.value}
      onChange={({ value }) => props.onChange(value)}
      placeholder={props.placeholder}
      searchable
      searchPlaceholder="Search providers"
      searchAccessibilityLabel="Search providers"
      searchTestID={props.testID ? `${props.testID}-search` : undefined}
      accessibilityLabel={props.accessibilityLabel}
      popupClass="rounded-(--radius) border border-border bg-popover p-[4px] shadow-lg"
      listClass="max-h-[340px]"
      itemClass="group rounded-(--radius-sm) px-(--space-2) py-[6px] focus:bg-accent disabled:opacity-50"
      searchClass="mb-[4px] rounded-(--radius-sm) border border-input bg-background px-(--space-2) py-(--space-1) text-(length:--text-sm) text-foreground"
      empty={<Text class="px-(--space-2) py-(--space-2) text-(length:--text-sm) text-muted-foreground">No provider matches.</Text>}
      testID={props.testID}
      renderItem={(entry) => {
        const item = byValue().get(entry.value);
        return (
          <View class="flex-row items-center gap-(--space-2)" testID={props.testID ? `${props.testID}:${entry.value}` : undefined}>
            <If when={item}>{(i) => <BrandMark mark={i().mark} />}</If>
            <View class="flex-1 min-w-0 flex-col">
              <Text class="text-(length:--text-sm) font-medium text-popover-foreground" style={{ numberOfLines: 1 }}>
                {item?.label ?? entry.title}
              </Text>
              <If when={item?.hint}>
                <Text class="text-(length:--text-xs) text-muted-foreground" style={{ numberOfLines: 1 }}>
                  {item?.hint}
                </Text>
              </If>
            </View>
          </View>
        );
      }}
      trigger={
        <View class="flex-1 flex-row items-center gap-(--space-2)">
          <If when={current()} else={<Icon name="providers" color="muted" />}>
            {(c) => <BrandMark mark={c().mark} />}
          </If>
          <Text class={cx("flex-1 text-(length:--text-sm)", current() ? "font-medium text-foreground" : "text-muted-foreground")} style={{ numberOfLines: 1 }}>
            {current()?.label ?? props.placeholder}
          </Text>
          <Icon name="chevron-down" color="muted" size={14} />
        </View>
      }
    />
  );
}

// ---------------------------------------------------------------- search box

/**
 * The app's search box: a magnifier, the field and an optional trailing
 * control (a mode). Return submits.
 */
export function SearchBox(
  props: Testable & {
    value: string;
    onChange: (value: string) => void;
    onSubmit: () => void;
    onFocus?: () => void;
    placeholder: string;
    maxLength?: number;
    trailing?: JSX.Element;
  },
) {
  return (
    <View class="flex-row items-center gap-(--space-2) w-full max-w-[560px] min-w-0 rounded-(--radius) border border-input bg-muted/60 pl-(--space-3) pr-[4px] focus-within:border-ring focus-within:bg-background">
      <Icon name="search" color="muted" />
      <Input
        class="flex-1 min-w-0 py-[7px] text-(length:--text-sm) text-foreground placeholder:text-muted-foreground"
        value={props.value}
        onChange={({ value }) => props.onChange(value)}
        onSubmit={() => props.onSubmit()}
        onFocus={() => props.onFocus?.()}
        submitBehavior="submit"
        placeholder={props.placeholder}
        maxLength={props.maxLength}
        testID={props.testID}
        accessibilityLabel={props.accessibilityLabel ?? props.placeholder}
      />
      {props.trailing}
    </View>
  );
}

import { Text } from "nsh";
import { Button } from "nsh/base";
import { Icon } from "./chrome";

/** Keep long provider/model ids from pushing Send outside a narrow composer. */
export function ComposerModelTrigger(props: { label: string; onPress: () => void }) {
  return (
    <Button class="flex-row max-w-[160px] h-[28px] items-center gap-(--space-1) rounded-(--radius-sm) px-(--space-2) hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring"
      testID="composer-model" accessibilityLabel={props.label} onPress={props.onPress}>
      <Text class="shrink text-(length:--text-xs) text-muted-foreground" style={{ numberOfLines: 1 }}>{props.label}</Text>
      <Icon name="chevron-down" size={14} color="muted" />
    </Button>
  );
}

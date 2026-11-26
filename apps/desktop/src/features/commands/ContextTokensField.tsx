import { Column, Field, Label } from "../../ui";

export const contextTokensError = (value: string) => {
  const text = value.trim();
  return !text || /^\d+$/.test(text) && Number(text) >= 1024 && Number(text) <= 10_000_000
    ? null : "Enter a whole number from 1,024 to 10,000,000, or leave blank.";
};

export function ContextTokensField(props: {
  testID: string; value: string; onChange: (value: string) => void; disabled?: boolean; onSubmit: () => void; onCancel?: () => void;
}) {
  return (
    <Column gap={1}>
      <Label variant="caption">Context size (tokens)</Label>
      <Field accessibilityLabel="Context size in tokens. Leave blank if unknown." testID={props.testID} value={props.value}
        onChange={props.onChange} placeholder="Unknown (assume 32,000)" disabled={props.disabled}
        onSubmit={props.onSubmit} onCancel={props.onCancel} error={contextTokensError(props.value)} />
      <If when={contextTokensError(props.value)}>
        {error => <Label variant="caption" color="danger" testID={`${props.testID}:error`}>{error()}</Label>}
      </If>
    </Column>
  );
}

import { mount, signal, View } from "nsh";
import { compactWidth, Label, PageLayout } from "../../../ui";

function Fixture() {
  const width = signal(0), firstBottom = signal(0), secondTop = signal(0), footerWidth = signal(0);
  return <PageLayout inset={compactWidth() ? 4 : 6} gap={4} footer={
    <View onLayout={({ bounds }) => footerWidth.set(bounds.size.width)}>
      <Label>{`Footer width: ${footerWidth()}`}</Label>
    </View>
  }>
    <View onLayout={({ bounds }) => {
      width.set(bounds.size.width);
      firstBottom.set(bounds.origin.y + bounds.size.height);
    }}>
      <Label>{`Content width: ${width()}`}</Label>
    </View>
    <View onLayout={({ bounds }) => secondTop.set(bounds.origin.y)}>
      <Label>{`Section gap: ${secondTop() - firstBottom()}`}</Label>
    </View>
  </PageLayout>;
}

mount(Fixture);

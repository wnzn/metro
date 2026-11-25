// Template: Daily brief. __TARGET__ becomes room="…" agent="…", or room="chat".
export default (
  <Page title="Daily brief" refresh="15m">
    <Data name="brief" source="agentAnswer" __TARGET__ block="ask" />
    <Section key="brief" title="Today">
      <AgentAnswer key="answer" from={brief} empty="No brief yet. Ask for one, or keep it fresh with a schedule." />
      <Button key="ask" label="Write today's brief" action="runAgent" __TARGET__
              prompt="Write a short daily brief: what happened since yesterday, what needs attention today, and any open questions." />
    </Section>
  </Page>
);

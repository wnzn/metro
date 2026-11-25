// Template: Daily brief. Space answers follow the room and agent, including
// Template: scheduled runs. The personal answer follows only its page button.
export default (
  <Page title="Daily brief" refresh="15m">
    <Data name="brief" source="agentAnswer" __TARGET__ __PERSONAL_BRIEF_BUTTON__ />
    <Section key="brief" title="Today">
      <AgentAnswer key="answer" from={brief} empty="No brief yet. Ask for one below." />
      <Button key="ask" label="Write today's brief" action="runAgent" __TARGET__
              prompt="Write a short daily brief: what happened since yesterday, what needs attention today, and any open questions." />
    </Section>
  </Page>
);

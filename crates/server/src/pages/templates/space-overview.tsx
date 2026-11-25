// Template: Space overview. __TARGET__ becomes room="…" agent="…".
export default (
  <Page title="Space overview" refresh="1m">
    <Data name="agents" source="agents" />
    <Data name="runs" source="runs" limit={10} />
    <Data name="latest" source="agentAnswer" __TARGET__ />
    <Row key="stats">
      <Stat key="active" label="Active runs" value={runs.active} />
    </Row>
    <Section key="agents" title="Agents">
      <List key="agent-list" items={agents.items} as="agent" empty="No agents">
        <Text key="agent-name">{agent.name} (@{agent.handle})</Text>
        <Text key="agent-description" muted>{agent.description}</Text>
      </List>
    </Section>
    <Section key="latest-answer" title="Latest answer">
      <AgentAnswer key="answer" from={latest} empty="No answers yet" />
    </Section>
    <Section key="recent-runs" title="Recent runs">
      <RunList key="run-list" from={runs} empty="No recent runs" />
    </Section>
  </Page>
);

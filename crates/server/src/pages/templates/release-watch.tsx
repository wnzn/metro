// Template: Release watch. __TARGET__ becomes room="…" agent="…".
export default (
  <Page title="Release watch" refresh="5m">
    <Data name="runs" source="runs" __ROOM_ONLY__ limit={10} />
    <Data name="status" source="agentAnswer" __TARGET__ block="ask" />
    <Row key="top">
      <Stat key="active" label="Active runs" value={runs.active} />
      <Stat key="last" label="Last status" value={status.at} format="relative" />
    </Row>
    <Section key="summary" title="Release status">
      <AgentAnswer key="answer" from={status} empty="Ask for a status update" />
      <Button key="ask" label="Ask for a status update" action="runAgent" __TARGET__
              prompt="Summarise the current release status: what is done, what is blocked, and what is next." />
    </Section>
    <Table key="run-table" rows={runs.items} empty="Nothing running">
      <Column field="agent" label="Agent" />
      <Column field="status" label="Status" format="status" />
      <Column field="started" label="Started" format="relative" />
    </Table>
  </Page>
);

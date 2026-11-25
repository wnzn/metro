// Template: My runs.
export default (
  <Page title="Runs" refresh="30s">
    <Data name="runs" source="runs" limit={50} />
    <Row key="top">
      <Stat key="active" label="Active runs" value={runs.active} tone="info" />
    </Row>
    <Table key="run-table" rows={runs.items} empty="No recent runs">
      <Column field="agent" label="Agent" />
      <Column field="room" label="Room" />
      <Column field="status" label="Status" format="status" />
      <Column field="sender" label="Started by" />
      <Column field="started" label="Started" format="relative" />
      <Column field="task" label="Task" />
    </Table>
  </Page>
);

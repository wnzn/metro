// Template: Files.
export default (
  <Page title="Files" refresh="15m">
    <Data name="files" source="library" limit={20} />
    <Section key="recent" title="Recent files">
      <FileList key="file-list" from={files} empty="No files yet" />
    </Section>
  </Page>
);

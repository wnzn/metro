// Settings › Profile: the signed-in user's display name, short bio and
// picture. Others see them only where they share a Space; the stable user id
// (what a Space owner adds) never changes with them.
//
// The page scrolls on its own; feedback and Save stay fixed beneath it, so
// they're in view on a compact window too.

import { Clipboard, effect, signal, untracked } from "nsh";
import { Action, ActionBar, Banner, Column, compactWidth, Field, Label, PageHeader, PageLayout, Row, SettingRow, SettingsGroup } from "../../ui";
import { MAX_AVATAR_BYTES, MAX_BIO, MAX_DISPLAY_NAME } from "../../api/profile";
import { me } from "../../store";
import { settingsAccountScope } from "../../settings";
import { PersonAvatar } from "./PersonAvatar";
import {
  bioDraft,
  choosePicture,
  loadMyProfile,
  myProfile,
  myProfileError,
  myProfileLoading,
  nameDraft,
  pictureBusy,
  profileDirty,
  profileNotice,
  profileProblem,
  removePicture,
  revertProfile,
  saveProfile,
  savingProfile,
} from "./profileState";

export function ProfileSettings() {
  effect(() => {
    if (me()) untracked(() => void loadMyProfile());
  });
  return (
    <PageLayout
      testID="profile-settings"
      pad={compactWidth() ? 4 : 6}
      gap={6}
      footer={
        <Column gap={2}>
          <Status />
          <If when={myProfile()}>
            <ActionBar justify="between" testID="profile-footer">
              <Column grow>
                <If
                  when={profileDirty() ? profileProblem() : null}
                  else={
                    <If when={profileDirty()}>
                      <Label variant="caption" color="muted" testID="profile-unsaved">
                        Unsaved changes
                      </Label>
                    </If>
                  }
                >
                  {(problem) => (
                    <Label variant="caption" color="danger" testID="profile-problem">
                      {problem()}
                    </Label>
                  )}
                </If>
              </Column>
              <Row gap={2}>
                <Action testID="profile-revert" label="Revert" variant="ghost"
                  disabled={!profileDirty() || savingProfile() || myProfileLoading()} onPress={revertProfile} />
                <Action testID="profile-save" label="Save" busyLabel="Saving…" variant="primary" busy={savingProfile()}
                  disabled={!profileDirty() || !!profileProblem() || myProfileLoading() || !!pictureBusy()} onPress={() => void saveProfile()} />
              </Row>
            </ActionBar>
          </If>
        </Column>
      }
    >
      <PageHeader title="Profile" subtitle="How you appear to people who share a Space with you." scope={settingsAccountScope()} />
      <If
        when={myProfile()}
        else={
          <If when={myProfileLoading()}>
            <SettingsGroup><SettingRow label="Loading your profile…" testID="profile-loading" /></SettingsGroup>
          </If>
        }
      >
        {(profile) => (
          <Column gap={6}>
            <SettingsGroup title="Picture" description={`Use a PNG or JPEG up to ${MAX_AVATAR_BYTES / (1024 * 1024)} MB; picture changes apply immediately.`}>
              <SettingRow label="Your picture" description="Without a picture, Metro shows your initials." stacked>
                <Row gap={4} align="center" wrap>
                  <PersonAvatar testID="profile-avatar" userId={profile().id} name={profile().displayName || profile().id} size="lg" accessibilityLabel="Your picture" />
                  <Row gap={2} wrap>
                    <Action
                      testID="profile-picture-choose"
                      label={profile().avatarVersion ? "Change picture…" : "Choose picture…"}
                      busyLabel="Uploading…"
                      busy={pictureBusy() === "upload"}
                      disabled={pictureBusy() !== null || savingProfile() || myProfileLoading()}
                      onPress={() => void choosePicture()}
                    />
                    <If when={profile().avatarVersion}>
                      <Action
                        testID="profile-picture-remove"
                        label="Remove picture"
                        busyLabel="Removing…"
                        variant="ghost"
                        busy={pictureBusy() === "remove"}
                        disabled={pictureBusy() !== null || savingProfile() || myProfileLoading()}
                        onPress={() => void removePicture()}
                      />
                    </If>
                  </Row>
                </Row>
              </SettingRow>
            </SettingsGroup>

            <SettingsGroup title="About you">
              <SettingRow label="Display name" description={`Shown on your messages, up to ${MAX_DISPLAY_NAME} characters.`} stacked>
                <Field
                  testID="profile-name"
                  accessibilityLabel="Display name"
                  value={nameDraft()}
                  onChange={nameDraft.set}
                  placeholder="Your name"
                  maxLength={MAX_DISPLAY_NAME}
                  disabled={savingProfile() || myProfileLoading()}
                />
              </SettingRow>
              <SettingRow label="Bio" description={`Optional, up to ${MAX_BIO} characters.`} stacked>
                <Field
                  testID="profile-bio"
                  accessibilityLabel="Bio"
                  value={bioDraft()}
                  onChange={bioDraft.set}
                  placeholder="A line about you"
                  multiline
                  maxLength={MAX_BIO}
                  disabled={savingProfile() || myProfileLoading()}
                />
              </SettingRow>
              <SettingRow label="User ID" description="Space owners add you with this. It doesn't change with your name." stacked>
                <UserId id={profile().id} />
              </SettingRow>
            </SettingsGroup>
          </Column>
        )}
      </If>
    </PageLayout>
  );
}

/** Load errors and what the last save or picture change did, in the fixed footer so they're seen. */
function Status() {
  return (
    <Column>
      <If when={myProfileError()}>
        {(message) => (
          <Banner
            variant="inline"
            testID="profile-error"
            tone="danger"
            message={message()}
            action={<Action testID="profile-retry" label="Try again" onPress={() => void loadMyProfile()} />}
          />
        )}
      </If>
      <If when={profileNotice()}>
        {(n) => <Banner variant="inline" testID="profile-notice" tone={n().ok ? "success" : "danger"} message={n().message} />}
      </If>
    </Column>
  );
}

/** The stable id, selectable, with a Copy button. */
function UserId(props: { id: string }) {
  const copied = signal(false);
  return (
    <Row gap={3} align="center" wrap>
      <Row selectable>
        <Label variant="mono" testID="profile-id">
          {props.id}
        </Label>
      </Row>
      <Action
        testID="profile-id-copy"
        label={copied() ? "Copied" : "Copy ID"}
        accessibilityLabel={copied() ? "User ID copied" : "Copy your user ID"}
        variant="ghost"
        onPress={() => {
          Clipboard.writeText(props.id)
            .then(() => copied.set(true))
            .catch(() => {});
        }}
      />
    </Row>
  );
}

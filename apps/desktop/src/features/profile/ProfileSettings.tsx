// Settings › Profile: the signed-in user's display name, short bio and
// picture. Others see them only where they share a Space; the stable user id
// (what a Space owner adds) never changes with them.

import { effect, untracked } from "nsh";
import {
  Action,
  Banner,
  Column,
  Field,
  Label,
  PageHeader,
  Row,
  ScrollArea,
  SettingRow,
  SettingsGroup,
  Surface,
} from "../../ui";
import { MAX_AVATAR_BYTES, MAX_BIO, MAX_DISPLAY_NAME } from "../../api/profile";
import { me } from "../../store";
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
  saveProfile,
  savingProfile,
} from "./profileState";

export function ProfileSettings() {
  effect(() => {
    if (me()) untracked(() => void loadMyProfile());
  });
  return (
    <ScrollArea pad={6} testID="profile-settings">
      <Surface variant="content" gap={6}>
        <PageHeader title="Profile" subtitle="How you appear to people who share a Space with you." />
        <If when={myProfileError()}>
          {(message) => (
            <Banner tone="danger" testID="profile-error">
              <Column grow>
                <Label color="danger">{message()}</Label>
              </Column>
              <Action testID="profile-retry" label="Try again" onPress={() => void loadMyProfile()} />
            </Banner>
          )}
        </If>
        <If
          when={myProfile()}
          else={
            <If when={myProfileLoading()}>
              <Label color="muted" testID="profile-loading">
                Loading your profile…
              </Label>
            </If>
          }
        >
          {(profile) => (
            <Column gap={6}>
              <SettingsGroup title="Picture" description={`PNG or JPEG, up to ${MAX_AVATAR_BYTES / (1024 * 1024)} MB. Without one, your initials show.`}>
                <Row gap={4} align="center" padX={4} padY={4}>
                  <PersonAvatar testID="profile-avatar" userId={profile().id} name={profile().displayName || profile().id} size="lg" accessibilityLabel="Your picture" />
                  <Column grow gap={2}>
                    <Row gap={2} wrap>
                      <Action
                        testID="profile-picture-choose"
                        label={profile().avatarVersion ? "Change picture…" : "Choose picture…"}
                        busyLabel="Uploading…"
                        busy={pictureBusy() === "upload"}
                        disabled={pictureBusy() !== null}
                        onPress={() => void choosePicture()}
                      />
                      <If when={profile().avatarVersion}>
                        <Action
                          testID="profile-picture-remove"
                          label="Remove"
                          busyLabel="Removing…"
                          variant="ghost"
                          busy={pictureBusy() === "remove"}
                          disabled={pictureBusy() !== null}
                          onPress={() => void removePicture()}
                        />
                      </If>
                    </Row>
                  </Column>
                </Row>
              </SettingsGroup>

              <SettingsGroup title="About you">
                <SettingRow label="Display name" description="Shown on your messages." stacked>
                  <Field
                    testID="profile-name"
                    accessibilityLabel="Display name"
                    value={nameDraft()}
                    onChange={nameDraft.set}
                    placeholder="Your name"
                    maxLength={MAX_DISPLAY_NAME}
                    disabled={savingProfile()}
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
                    disabled={savingProfile()}
                  />
                </SettingRow>
                <SettingRow label="User ID" description="Space owners add you with this. It doesn't change with your name." stacked>
                  <Row selectable>
                    <Label variant="mono" testID="profile-id">
                      {profile().id}
                    </Label>
                  </Row>
                </SettingRow>
              </SettingsGroup>

              <Row gap={2} align="center">
                <Column grow>
                  <If when={profileDirty() ? profileProblem() : null}>
                    {(problem) => (
                      <Label variant="caption" color="danger" testID="profile-problem">
                        {problem()}
                      </Label>
                    )}
                  </If>
                </Column>
                <Action
                  testID="profile-save"
                  label="Save"
                  busyLabel="Saving…"
                  variant="primary"
                  busy={savingProfile()}
                  disabled={!profileDirty() || !!profileProblem()}
                  onPress={() => void saveProfile()}
                />
              </Row>
            </Column>
          )}
        </If>
        <If when={profileNotice()}>
          {(n) => (
            <Label variant="caption" color={n().ok ? "success" : "danger"} testID="profile-notice">
              {n().message}
            </Label>
          )}
        </If>
      </Surface>
    </ScrollArea>
  );
}

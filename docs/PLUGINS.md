# Plugins

Ashcord can change Discord in three places. Which one a plugin needs decides when it can exist:

| Layer | What it touches | Works with | Status |
|---|---|---|---|
| **Network** | API requests, API responses, gateway events, what the client sends | any client, today's and Phoenix | live (v0.1) |
| **Theme** | colours, fonts, spacing | Phoenix, if its themes are data files (GPUI/Zed style) | waits for Phoenix |
| **Hooks** | the client's own code: buttons, menus, panels | Phoenix, per release | waits for Phoenix |

Today's Electron client already has Vencord for the theme and hook layers. Ashcord's network plugins
work alongside it, so you can run both.

## Shipped (network layer)

| Ashcord | Vencord | What it does |
|---|---|---|
| `no_track` | NoTrack | Answers Discord's analytics (`/science`, `/metrics`) and crash reports locally, so nothing is sent. On by default. |
| `silent_typing` | SilentTyping | Your typing indicator is never sent. |
| `message_logger` | MessageLogger | Deleted messages stay on screen under a "deleted" line; edited messages show their earlier versions. Saved to `logs/messages.jsonl` too (`ashcord log`). Your own messages are ignored by default. |
| `fake_nitro` | FakeNitro | Emoji you can't use (other servers, animated, DMs) are sent as small linked images. The pickers are unlocked by telling the client it has Nitro. |
| `relationship_notifier` | RelationshipNotifier | Desktop notification when someone unfriends you, cancels or declines a friend request, or removes you from a server or group DM. Your own actions aren't reported. |
| `game_activity_toggle` | GameActivityToggle | While on, the game you're playing isn't in your status. `ashcord toggle game_activity_toggle` flips it and re-sends your status at once. |
| `unindent` | Unindent | Strips the shared indentation from code blocks you send. |

## Planned

**Network layer, doable now:** iLoveSpam (clear the spammer flag so those messages show), ShowHiddenChannels
(list channels you can't open), Translate (incoming text through a translation backend you choose),
MessageLinkEmbeds (inline previews of linked messages), FixYoutubeEmbeds / FixSpotifyEmbeds (embed
rewriting), FriendInvites and petpet as `ashcord` commands, Fake Nitro stickers and stream quality.

**Theme layer (Phoenix):** FixDiscordCss, FixCodeblockGap, ShikiCodeblocks-style highlighting, NoMosaic,
custom themes.

**Hook layer (Phoenix):** everything that adds UI: PermissionsViewer, ServerInfo, MemberCount,
PlatformIndicators, ShowTimeoutDuration, FriendsSince, MutualGroupDMs, ViewIcons, ImageZoom,
UnlockedAvatarZoom, PictureInPicture, BiggerStreamPreview, CallTimer, DisableCallIdle, VolumeBooster,
VoiceMessages, SpotifyControls, ReviewDB, BetterSessions, BetterSettings, MessageClickActions,
FavoriteGifSearch, GifPaste, GreetStickerPicker, AlwaysExpandRoles, ForceOwnerCrown, FullUserInChatbox,
MentionAvatars, TypingIndicator, TypingTweaks, NoTypingAnimation, ShowHiddenThings, ImplicitRelationships,
OpenInApp, NoF1, BetterGifAltText, PermissionFreeWill, SilentMessageToggle.

**Not needed / out of scope:** the Vencord `*API` plugins, Settings, SupportHelper, CrashHandler and
ConcatenatedComponentExtractor (Vencord internals); YoutubeAdblock and FixImagesQuality (web-client
quirks); SpotifyCrack (Spotify-side, not Discord's); ValidUser (web-client cache quirk).

## Writing a plugin

A plugin is a Rust type implementing `plugins::Plugin` (see `src/plugins/mod.rs`): it can answer or
change API requests, read API responses, change, add or drop gateway events, and change what the client
sends. Gateway payloads arrive as one `Val` type whether the client uses JSON or ETF, so a plugin is
written once. Each plugin gets a section in `config.toml` and tests next to its code.

//! Example demonstrating how to make use of individual track audio events,
//! and how to use the `TrackQueue` system.
//!
//! Requires the "cache", "standard_framework", and "voice" features be enabled in your
//! Cargo.toml, like so:
//!
//! ```toml
//! [dependencies.serenity]
//! git = "https://github.com/serenity-rs/serenity.git"
//! features = ["cache", "framework", "standard_framework", "voice"]
//! ```
use std::{
    env,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use serenity::{
    async_trait,
    client::{Client, Context, EventHandler},
    framework::{
        standard::{
            macros::{command, group},
            Args, CommandResult,
        },
        StandardFramework,
    },
    http::Http,
    model::{
        channel::Message,
        gateway::{Activity, Ready},
        id::GuildId,
        misc::Mentionable,
        prelude::ChannelId,
    },
    prelude::{RwLock, TypeMapKey},
    Result as SerenityResult,
};

use songbird::{
    events::EventStore,
    input::{self, restartable::Restartable},
    input::{Input, Metadata},
    tracks::{self, LoopState, TrackError},
    Call, Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit, TrackEvent,
};
use youtube_dl::{YoutubeDl, YoutubeDlOutput};

static ICON: &'static str =
    "https://cdn.discordapp.com/avatars/887241846869360641/70525dd8fab9290f78cc7ad2e26728a6.webp";
static EMBED_COLOUR: (u8, u8, u8) = (253, 195, 213);
struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
        ctx.set_activity(Activity::watching("your mother")).await;
    }
}

#[group]
#[commands(
    join, leave, play_fade, play, play_playlist,/*queue,*/ skip, stop, ping, nowplaying, songloop
)]
struct General;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

    let framework = StandardFramework::new()
        .configure(|c| c.prefix("~"))
        .group(&GENERAL_GROUP);

    let mut client = Client::builder(&token)
        .event_handler(Handler)
        .framework(framework)
        .register_songbird()
        .await
        .expect("Err creating client");

    let _ = client
        .start()
        .await
        .map_err(|why| println!("Client ended: {:?}", why));
}

#[command]
#[only_in(guilds)]
async fn join(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).await.unwrap();
    let guild_id = guild.id;

    let channel_id = guild
        .voice_states
        .get(&msg.author.id)
        .and_then(|voice_state| voice_state.channel_id);

    let connect_to = match channel_id {
        Some(channel) => channel,
        None => {
            check_msg(msg.reply(ctx, "Not in a voice channel").await);

            return Ok(());
        }
    };

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let (handle_lock, success) = manager.join(guild_id, connect_to).await;

    if let Ok(_channel) = success {
        check_msg(
            msg.channel_id
                .say(&ctx.http, &format!("Joined {}", connect_to.mention()))
                .await,
        );

        let chan_id = msg.channel_id;

        let send_http = ctx.http.clone();

        let mut handle = handle_lock.lock().await;
        match handle.deafen(true).await {
            Ok(_) => {}
            Err(_) => check_msg(msg.channel_id.say(&ctx.http, "There was an error while trying to deafen, vivian didn't care enough to handle this, if this keeps happening and you can't fix it contact her").await),
        }

        handle.add_global_event(
            Event::Track(TrackEvent::End),
            TrackEndNotifier {
                chan_id,
                http: send_http,
            },
        );

        //let send_http = ctx.http.clone();

        /*
        handle.add_global_event(
            Event::Periodic(Duration::from_secs(60), None),
            ChannelDurationNotifier {
                chan_id,
                count: Default::default(),
                http: send_http,
            },
        );
        */
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Error joining the channel")
                .await,
        );
    }

    Ok(())
}

struct TrackEndNotifier {
    chan_id: ChannelId,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for TrackEndNotifier {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(track_list) = ctx {
            check_msg(
                self.chan_id
                    .say(&self.http, &format!("Tracks ended: {}.", track_list.len()))
                    .await,
            );
        }

        None
    }
}

/*
struct ChannelDurationNotifier {
    chan_id: ChannelId,
    count: Arc<AtomicUsize>,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for ChannelDurationNotifier {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        let count_before = self.count.fetch_add(1, Ordering::Relaxed);
        check_msg(
            self.chan_id
                .say(
                    &self.http,
                    &format!(
                        "I've been in this channel for {} minutes!",
                        count_before + 1
                    ),
                )
                .await,
        );

        None
    }
}
*/

#[command]
#[only_in(guilds)]
async fn leave(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).await.unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();
    let has_handler = manager.get(guild_id).is_some();

    if has_handler {
        if let Err(e) = manager.remove(guild_id).await {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, format!("Failed: {:?}", e))
                    .await,
            );
        }

        check_msg(msg.channel_id.say(&ctx.http, "Left voice channel").await);
    } else {
        check_msg(msg.reply(ctx, "Not in a voice channel").await);
    }

    Ok(())
}

#[command]
async fn ping(ctx: &Context, msg: &Message) -> CommandResult {
    check_msg(msg.channel_id.say(&ctx.http, "Pong!").await);

    Ok(())
}

#[command]
#[only_in(guilds)]
#[aliases("loop")]
async fn songloop(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).await.unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let mut enable_loop = false;

    if let Some(handler_lock) = manager.get(guild_id) {
        let handler = handler_lock.lock().await;
        let queue = handler.queue();
        let current_song = match queue.current() {
            Some(song) => song,
            None => {
                check_msg(
                    msg.channel_id
                        .say(
                            &ctx.http,
                            "No song is playing, please, i beg you, play a song, ᵖˡᵉᵃˢᵉ",
                        )
                        .await,
                );
                return Ok(());
            }
        };

        match current_song.get_info().await {
            Ok(info) => {
                if info.loops != LoopState::Infinite {
                    enable_loop = true;
                }
            }
            Err(e) => match e {
                TrackError::Finished => check_msg(
                    msg.channel_id
                        .say(
                            &ctx.http,
                            "The song is already finished, i beg you, ᵖˡᵉᵃˢᵉ ᵠᵘᵉᵘᵉ ᵃⁿᵒᵗʰᵉʳ ᵒⁿᵉ",
                        )
                        .await,
                ),
                _ => check_msg(
                    msg.channel_id
                        .say(
                            &ctx.http,
                            "ok the dev actually done goofed, contact here and make fun of her (1)",
                        )
                        .await,
                ),
            },
        };

        if enable_loop {
            match current_song.enable_loop() {
                Ok(_) => check_msg(msg.channel_id.say(&ctx.http, "Enabled infinite loop for the current song!").await),
                Err(e) => match e {
                    TrackError::Finished => check_msg(
                        msg.channel_id
                            .say(
                                &ctx.http,
                                "The song is already finished, i beg you, ᵖˡᵉᵃˢᵉ ᵠᵘᵉᵘᵉ ᵃⁿᵒᵗʰᵉʳ ᵒⁿᵉ",
                            )
                            .await,
                    ),
                    _ => check_msg(
                        msg.channel_id
                            .say(
                                &ctx.http,
                                "ok the dev actually done goofed, contact here and make fun of her (2)",
                            )
                            .await,
                    ),
                },
            }
        } else {
            match current_song.disable_loop() {
                Ok(_) => check_msg(msg.channel_id.say(&ctx.http, "Disabled infinite loop for the current song!").await),
                Err(e) => match e {
                    TrackError::Finished => check_msg(
                        msg.channel_id
                            .say(
                                &ctx.http,
                                "The song is already finished, i beg you, ᵖˡᵉᵃˢᵉ ᵠᵘᵉᵘᵉ ᵃⁿᵒᵗʰᵉʳ ᵒⁿᵉ",
                            )
                            .await,
                    ),
                    _ => check_msg(
                        msg.channel_id
                            .say(
                                &ctx.http,
                                "ok the dev actually done goofed, contact here and make fun of her (2)",
                            )
                            .await,
                    ),
                },
            }
        }
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
#[aliases("np")]
async fn nowplaying(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).await.unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let channel_id = guild
        .voice_states
        .get(&msg.author.id)
        .and_then(|voice_state| voice_state.channel_id);

    let chan = match channel_id {
        Some(channel) => channel,
        None => {
            check_msg(msg.reply(ctx, "You are not in a vc.").await);

            return Ok(());
        }
    };

    if let Some(handler_lock) = manager.get(guild_id) {
        let handler = handler_lock.lock().await;
        /*
         * basic ~queue logic
        let mut queue_str = String::new();

        let queue = handler.queue().current_queue();

        for (n, i) in queue.iter().enumerate() {
            let title = i.metadata().title.clone().unwrap_or("<no title>".into());
            queue_str.push_str(&format!("[{n}] {title}\n"))
        }
        */

        let songtitle;
        let thumblink;
        let statusbar;
        let duration;

        let mut np_str = String::new();

        if let Some(current) = handler.queue().current() {
            let md = current.metadata().clone();
            songtitle = md.title.unwrap_or("<no title>".into());
            thumblink = md.thumbnail;
            let curpos = match current.get_info().await {
                Ok(state) => state.position,
                Err(e) => match e {
                    TrackError::Finished => {
                        check_msg(
                            msg.channel_id
                                .say(
                                    &ctx.http,
                                    "The song is finished and there are no more songs",
                                )
                                .await,
                        );
                        return Ok(());
                    }
                    _ => {
                        check_msg(
                            msg.channel_id
                                .say(
                                    &ctx.http,
                                    "This error shouldn't occur? please contact the developer (err 1)",
                                )
                                .await,
                        );
                        return Ok(());
                    }
                },
            };

            np_str.push_str(":arrow_forward: ");

            // 13 dynamic symbols
            // formula: (current seconds/duration seconds)*13
            // current point is the round up of formula, fill the rest

            // i think its safe to unwrap
            let current_pointer =
                ((curpos.as_secs_f64() / md.duration.unwrap().as_secs_f64()) * 13.).ceil();

            let before = current_pointer - 1.;
            let after = 13. - current_pointer;

            for _ in 0..=before as u64 {
                np_str.push('▬');
            }

            // the current pointer
            np_str.push_str(":radio_button:");

            for _ in 0..=after as u64 {
                np_str.push('▬');
            }

            np_str.push(' ');

            duration = format!(
                "[{}/{}]",
                hrtime::from_sec_padded(curpos.as_secs()),
                hrtime::from_sec_padded(md.duration.unwrap().as_secs())
            );

            np_str.push_str(":loud_sound:");

            statusbar = np_str;

            check_msg(
                msg.channel_id
                    .send_message(&ctx.http, |m| {
                        m.content(format!("Now playing (in {}): ", chan.mention()));
                        m.embed(|e| {
                            e.colour(EMBED_COLOUR)
                                .title(songtitle)
                                .thumbnail(thumblink.unwrap_or(ICON.into()))
                                .description(statusbar)
                                .footer(|f| {
                                    f.text(format!("Duration: {}", duration)).icon_url(ICON)
                                })
                        })
                    })
                    .await,
            );
        } else {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, ":x: there is literally no song playing rn")
                    .await,
            );
        }
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn play_playlist(ctx: &Context, msg: &Message, args: Args) -> CommandResult {
    let query = args.raw().collect::<Vec<&str>>().join(" ");
    if ((query == "") || !(query.starts_with("http"))) && !(query.contains("playlist")) {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Must provide a playlist URL")
                .await,
        );
        return Ok(());
    }

    check_msg(msg.channel_id.say(&ctx.http, "Polling...").await);
    let output =
        tokio::task::spawn_blocking(move || YoutubeDl::new(query).socket_timeout("15").run())
            .await
            .unwrap()
            .unwrap(); // NOTE: DANGER
    check_msg(msg.channel_id.say(&ctx.http, "Polled!").await);

    let videos = match output {
        YoutubeDlOutput::Playlist(playlist) => match playlist.entries {
            Some(entryvec) => {
                let mut videos: Vec<String> = Vec::with_capacity(10);
                for i in entryvec {
                    videos.push(format!("https://youtube.com/watch?v={}", i.id));
                }
                videos
            }
            None => {
                async {
                    check_msg(
                        msg.channel_id
                            .say(&ctx.http, "This playlist has no videos!")
                            .await,
                    );
                }
                .await;

                return Ok(());
            }
        },
        _ => {
            async {
                    check_msg(
                        msg.channel_id
                            .say(&ctx.http, "THIS ISN'T EVEN A PLAYLIST, istg i filtered it, this shouldn't happen...")
                            .await,
                    );
            }.await;

            return Ok(());
        }
    };

    let guild = msg.guild(&ctx.cache).await.unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;
        for i in videos {
            queue_with_prebuf(SongType::Url(i), ctx, msg, &mut handler).await;
        }
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}

enum SongType {
    Url(String),
    Search(String),
}

async fn queue_with_prebuf(
    song: SongType,
    ctx: &Context,
    msg: &Message,
    handler: &mut Call,
) -> Option<Metadata> {
    let guild = msg.guild(&ctx.cache).await.unwrap();
    let guild_id = guild.id;

    match song {
        SongType::Url(url) => {
            // NOTE: this is not lazy
            let source = match Restartable::ytdl(url, false).await {
                Ok(source) => source,
                Err(why) => {
                    println!("Err starting source: {:?}", why);

                    check_msg(
                        msg.channel_id
                            .say(&ctx.http, "Error sourcing ffmpeg (see console)")
                            .await,
                    );

                    return None;
                }
            };

            let input: Input = source.into();

            let metadata = Some(*input.metadata.clone());

            // This handler object will allow you to, as needed,
            // control the audio track via events and further commands.
            handler.enqueue_source(input);
            if handler.queue().len() < 2 {
                handler.queue().pause().unwrap();

                let send_http = ctx.http.clone();
                let chan_id = msg.channel_id;

                check_msg(chan_id.say(&ctx.http, "Prebuffering...").await);

                let _ = handler.add_global_event(
                    Event::Delayed(Duration::from_secs(15)),
                    SongResumer {
                        guild_id,
                        context: ctx.clone(),
                        chan_id,
                        http: send_http,
                    },
                );
            }

            metadata
        }
        SongType::Search(search) => {
            // NOTE: this is not lazy
            let source = match Restartable::ytdl_search(search, false).await {
                Ok(source) => source,
                Err(why) => {
                    println!("Err starting source: {:?}", why);

                    check_msg(
                        msg.channel_id
                            .say(&ctx.http, "Error sourcing ffmpeg (see console)")
                            .await,
                    );

                    return None;
                }
            };

            let input: Input = source.into();

            let metadata = Some(*input.metadata.clone());

            handler.enqueue_source(input);
            if handler.queue().len() < 2 {
                handler.queue().pause().unwrap();

                let send_http = ctx.http.clone();
                let chan_id = msg.channel_id;

                check_msg(chan_id.say(&ctx.http, "Prebuffering...").await);

                let _ = handler.add_global_event(
                    Event::Delayed(Duration::from_secs(15)),
                    SongResumer {
                        guild_id,
                        context: ctx.clone(),
                        chan_id,
                        http: send_http,
                    },
                );
            }

            metadata
        }
    }
}

#[command]
#[only_in(guilds)]
async fn play(ctx: &Context, msg: &Message, /*mut*/ args: Args) -> CommandResult {
    let query = args.raw().collect::<Vec<&str>>().join(" ");
    if query == "" {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Must provide a URL or a search query")
                .await,
        );

        return Ok(());
    }

    let guild = msg.guild(&ctx.cache).await.unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;

        let mut metadata: Option<Metadata> = None;

        if query.starts_with("http") {
            let url;
            if query.find(" ").is_some() {
                url = query[0..query.find(" ").unwrap() + 1].to_string();
            } else {
                url = query.to_string();
            }

            metadata = match queue_with_prebuf(SongType::Url(url), ctx, msg, &mut handler).await {
                None => return Ok(()),
                Some(m) => Some(m),
            }
        } else {
            metadata =
                match queue_with_prebuf(SongType::Search(query), ctx, msg, &mut handler).await {
                    None => return Ok(()),
                    Some(m) => Some(m),
                }
        }

        check_msg(
            msg.channel_id
                .send_message(&ctx.http, |m| {
                    m.embed(|e| {
                        if metadata.is_none() {
                            e.title("there was no metadata")
                                .description("how the fuck did this happen/")
                        } else {
                            let metadata = metadata.unwrap(); // safe to unwrap because we checked, i hope i dont regret this

                            e.colour(EMBED_COLOUR)
                                .title(metadata.title.unwrap_or("<no title> (how?????????)".into()))
                                .thumbnail(metadata.thumbnail.unwrap_or(ICON.into()))
                                .description(format!(
                                    "Added song to queue, position `{}`",
                                    handler.queue().len()
                                ))
                                .footer(|f| {
                                    f.text(format!(
                                        "Duration: {}",
                                        hrtime::from_sec_padded(
                                            metadata
                                                .duration
                                                .unwrap_or(Duration::from_secs(0))
                                                .as_secs()
                                        )
                                    ))
                                    .icon_url(ICON)
                                })
                        }
                    })
                })
                .await,
        );
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}

struct SongResumer {
    guild_id: GuildId,
    context: Context,
    chan_id: ChannelId,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for SongResumer {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(&[(state, track)]) = ctx {
            let manager = songbird::get(&self.context)
                .await
                .expect("Songbird Voice client placed in at initialisation")
                .clone();
            println!("reached");

            if let Some(handler_lock) = manager.get(self.guild_id) {
                let handler = handler_lock.lock().await;
                handler.queue().resume().unwrap();
                Some(Event::Cancel)
            } else {
                // i do not know kiel je fek this could happen but here goes
                check_msg(
                    self.chan_id
                        .say(&self.http, "Not in a voice channel to play in")
                        .await,
                );
                None
            }
        } else {
            None
        }
    }
}

#[command]
#[only_in(guilds)]
async fn play_fade(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let url = match args.single::<String>() {
        Ok(url) => url,
        Err(_) => {
            check_msg(
                msg.channel_id
                    .say(&ctx.http, "Must provide a URL to a video or audio")
                    .await,
            );

            return Ok(());
        }
    };

    if !url.starts_with("http") {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Must provide a valid URL")
                .await,
        );

        return Ok(());
    }

    let guild = msg.guild(&ctx.cache).await.unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;

        let source = match input::ytdl(&url).await {
            Ok(source) => source,
            Err(why) => {
                println!("Err starting source: {:?}", why);

                check_msg(msg.channel_id.say(&ctx.http, "Error sourcing ffmpeg").await);

                return Ok(());
            }
        };

        // This handler object will allow you to, as needed,
        // control the audio track via events and further commands.
        let song = handler.play_source(source);
        let send_http = ctx.http.clone();
        let chan_id = msg.channel_id;

        // This shows how to periodically fire an event, in this case to
        // periodically make a track quieter until it can be no longer heard.
        let _ = song.add_event(
            Event::Periodic(Duration::from_secs(5), Some(Duration::from_secs(7))),
            SongFader {
                chan_id,
                http: send_http,
            },
        );

        let send_http = ctx.http.clone();

        // This shows how to fire an event once an audio track completes,
        // either due to hitting the end of the bytestream or stopped by user code.
        let _ = song.add_event(
            Event::Track(TrackEvent::End),
            SongEndNotifier {
                chan_id,
                http: send_http,
            },
        );

        check_msg(msg.channel_id.say(&ctx.http, "Playing song").await);
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}

struct SongFader {
    chan_id: ChannelId,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for SongFader {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(&[(state, track)]) = ctx {
            let _ = track.set_volume(state.volume / 2.0);

            if state.volume < 1e-2 {
                let _ = track.stop();
                check_msg(self.chan_id.say(&self.http, "Stopping song...").await);
                Some(Event::Cancel)
            } else {
                check_msg(self.chan_id.say(&self.http, "Volume reduced.").await);
                None
            }
        } else {
            None
        }
    }
}

struct SongEndNotifier {
    chan_id: ChannelId,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for SongEndNotifier {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        check_msg(
            self.chan_id
                .say(&self.http, "Song faded out completely!")
                .await,
        );

        None
    }
}

/*
#[command]
#[only_in(guilds)]
async fn queue(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let guild = msg.guild(&ctx.cache).await.unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;

        // Here, we use lazy restartable sources to make sure that we don't pay
        // for decoding, playback on tracks which aren't actually live yet.
        let source = match Restartable::ytdl(url, true).await {
            Ok(source) => source,
            Err(why) => {
                println!("Err starting source: {:?}", why);

                check_msg(msg.channel_id.say(&ctx.http, "Error sourcing ffmpeg").await);

                return Ok(());
            }
        };

        handler.enqueue_source(source.into());

        check_msg(
            msg.channel_id
                .say(
                    &ctx.http,
                    format!("Added song to queue: position {}", handler.queue().len()),
                )
                .await,
        );
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}
*/

#[command]
#[only_in(guilds)]
async fn skip(ctx: &Context, msg: &Message, _args: Args) -> CommandResult {
    let guild = msg.guild(&ctx.cache).await.unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let handler = handler_lock.lock().await;
        let queue = handler.queue();
        let _ = queue.skip();

        check_msg(
            msg.channel_id
                .say(
                    &ctx.http,
                    format!("Song skipped: {} in queue.", queue.len()),
                )
                .await,
        );
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn stop(ctx: &Context, msg: &Message, _args: Args) -> CommandResult {
    let guild = msg.guild(&ctx.cache).await.unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    if let Some(handler_lock) = manager.get(guild_id) {
        let handler = handler_lock.lock().await;
        let queue = handler.queue();
        let _ = queue.stop();

        check_msg(msg.channel_id.say(&ctx.http, "Queue cleared.").await);
    } else {
        check_msg(
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await,
        );
    }

    Ok(())
}

/// Checks that a message successfully sent; if not, then logs why to stdout.
fn check_msg(result: SerenityResult<Message>) {
    if let Err(why) = result {
        println!("Error sending message: {:?}", why);
    }
}

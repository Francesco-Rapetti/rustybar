// The helper of now_playing's `other_apps`: what any app plays (a browser, Podcasts…), from
// MediaRemote, the private framework of Control Center. Since macOS 15.4 MediaRemote answers only
// Apple's own processes, so RustyBar can't load this itself: /usr/bin/perl, signed by Apple, loads
// it and calls rustybar_now_playing, which never returns (see `spawn_helper` in now_playing.rs).
// build.rs compiles it, and RustyBar carries it inside its binary.
//
// It reports every app with a player MediaRemote knows (its clients), not only the one it calls
// playing: with Spotify playing over a video, the video's browser is still there. Its commands,
// though, reach only that one, the `active` app: macOS sends a command to the app it calls playing
// whatever player it names (tried with a full player path, as Control Center has one app too), so
// a command for another app is dropped here rather than landing on the wrong one.
//
// Out, on stdout: a line of JSON with all of them ({"players": […]}) at every change of an app, a
// state, a track or the commands an app takes, or when a position isn't where it should be (a
// seek); then, in a line of its own, the cover of an app's artwork, base64. In, on stdin: a
// command per line for an app: `toggle <bundle>`, `next <bundle>`, `previous <bundle>`,
// `seek <bundle> <seconds>`. The end of stdin, RustyBar quitting or reloading, ends it.
//
// It waits for MediaRemote's notifications, without timers: at rest it does nothing.

#import <Foundation/Foundation.h>
#import <objc/message.h>
#include <dlfcn.h>
#include <math.h>
#include <stdio.h>

// MediaRemote's commands (MRMediaRemoteCommand) and playback states
enum { PLAY = 0, PAUSE = 1, TOGGLE = 2, NEXT = 4, PREVIOUS = 5, SEEK = 24 };
enum { PLAYING = 1 };

typedef void (^Done)(void);

static void (*getClients)(dispatch_queue_t, void (^)(NSArray *));
static void (*getState)(id, dispatch_queue_t, void (^)(unsigned));
static void (*getCommands)(id, dispatch_queue_t, void (^)(id));
static void (*requestQueue)(id, id, dispatch_queue_t, void (^)(id, NSError *));
static void (*sendCommand)(int, NSDictionary *, id, long, dispatch_queue_t, void (^)(id));
static NSString *positionOption; // kMRMediaRemoteOptionPlaybackPosition
static Class PlayerPath, QueueRequest, NowPlaying;
static id origin; // this Mac

static NSDictionary *paths;  // the player of each app, by bundle, from the last update
static NSArray *last;        // the players last sent, without their positions
static NSDictionary *moving; // bundle → @[position, at, rate] last sent, to see a seek
static NSMutableDictionary *covers; // bundle → the artwork whose cover was sent or asked for
static BOOL pending;         // an update is on its way
static unsigned long updates; // the latest: an answer to an older one is dropped

static void emit(NSDictionary *d) {
  NSData *json = [NSJSONSerialization dataWithJSONObject:d options:0 error:nil];
  if (!json) return;
  fwrite(json.bytes, 1, json.length, stdout);
  fputc('\n', stdout);
  if (fflush(stdout) != 0) exit(0); // RustyBar is gone
}

// A private property, or nil when the class doesn't have it (another macOS)
static id get(id o, NSString *key) {
  if (!o) return nil;
  @try {
    return [o valueForKey:key];
  } @catch (id e) {
    return nil;
  }
}

static NSString *string(id v) { return [v isKindOfClass:NSString.class] ? v : @""; }

static double number(id v) {
  double d = [v respondsToSelector:@selector(doubleValue)] ? [v doubleValue] : 0;
  return isfinite(d) ? d : 0;
}

// The app of a client: a web page plays in a helper process of its browser, the browser is the app
static NSString *bundleOf(id client) {
  NSString *bundle = string(get(client, @"parentApplicationBundleIdentifier"));
  return bundle.length ? bundle : string(get(client, @"bundleIdentifier"));
}

// The app MediaRemote calls playing, which gets the commands
static NSString *activeBundle(void) {
  return bundleOf(get(((id (*)(id, SEL))objc_msgSend)(NowPlaying, @selector(localNowPlayingPlayerPath)), @"client"));
}

static id queueRequest(double artwork) {
  id request = ((id (*)(id, SEL))objc_msgSend)(QueueRequest, @selector(defaultPlaybackQueueRequest));
  ((void (*)(id, SEL, double))objc_msgSend)(request, @selector(setArtworkWidth:), artwork);
  ((void (*)(id, SEL, double))objc_msgSend)(request, @selector(setArtworkHeight:), artwork);
  return request;
}

// The cover of an app's item, 320 px, sent with its artwork's id: the players went first.
static void requestCover(NSString *bundle, NSString *artwork, id path) {
  requestQueue(queueRequest(320), path, dispatch_get_main_queue(), ^(id queue, NSError *error) {
    id item = [get(queue, @"contentItems") firstObject];
    NSData *data = get(get(item, @"artwork"), @"imageData");
    NSString *cover = [data isKindOfClass:NSData.class] ? [data base64EncodedStringWithOptions:0] : @"";
    emit(@{@"bundle" : bundle, @"artwork" : artwork, @"cover" : cover});
  });
}

// One app: its state, commands and item, each asked of MediaRemote within `group`.
static NSMutableDictionary *ask(id client, id path, dispatch_group_t group) {
  NSMutableDictionary *p = [@{
    @"bundle" : bundleOf(client),
    @"name" : string(get(client, @"displayName")),
    @"pid" : @((int)number(get(client, @"processIdentifier"))),
  } mutableCopy];
  dispatch_group_enter(group);
  getState(path, dispatch_get_main_queue(), ^(unsigned state) {
    p[@"playing"] = @(state == PLAYING);
    dispatch_group_leave(group);
  });
  dispatch_group_enter(group);
  getCommands(path, dispatch_get_main_queue(), ^(id commands) {
    NSArray *list = get(commands, @"command");
    if (![list isKindOfClass:NSArray.class]) list = @[];
    p[@"next"] = @([list containsObject:@(NEXT)]);
    p[@"previous"] = @([list containsObject:@(PREVIOUS)]);
    p[@"seek"] = @([list containsObject:@(SEEK)]);
    dispatch_group_leave(group);
  });
  dispatch_group_enter(group);
  requestQueue(queueRequest(0), path, dispatch_get_main_queue(), ^(id queue, NSError *error) {
    id md = get([get(queue, @"contentItems") firstObject], @"metadata");
    p[@"title"] = string(get(md, @"title"));
    p[@"artist"] = string(get(md, @"trackArtistName"));
    p[@"album"] = string(get(md, @"albumName"));
    p[@"duration"] = @(number(get(md, @"duration")));
    p[@"elapsed"] = @(number(get(md, @"elapsedTime")));
    // A CFAbsoluteTime (seconds since 2001), though its description shows a date
    id stamp = get(md, @"elapsedTimeTimestamp");
    p[@"stamp"] = @([stamp isKindOfClass:NSDate.class] ? [stamp timeIntervalSinceReferenceDate] : number(stamp));
    p[@"rate"] = @(number(get(md, @"playbackRate")));
    NSString *artwork = string(get(md, @"artworkIdentifier"));
    if (number(get(md, @"artworkAvailable")) == 0) artwork = @"";
    else if (!artwork.length) artwork = [NSString stringWithFormat:@"%@\x1f%@", p[@"title"], p[@"artist"]];
    p[@"artwork"] = artwork;
    dispatch_group_leave(group);
  });
  return p;
}

// All the players asked, in MediaRemote's order: sent if something changed.
static void report(NSArray *asked, NSDictionary *players, unsigned long update) {
  if (update != updates) return;
  paths = players;
  NSString *active = activeBundle();
  double now = CFAbsoluteTimeGetCurrent();
  NSMutableArray *list = [NSMutableArray array], *states = [NSMutableArray array];
  NSMutableDictionary *nowMoving = [NSMutableDictionary dictionary];
  BOOL jumped = NO;
  for (NSMutableDictionary *p in asked) {
    if (!p[@"title"] || !p[@"playing"]) continue; // an answer that never came
    BOOL playing = [p[@"playing"] boolValue];
    double rate = playing ? MAX([p[@"rate"] doubleValue], 0) : 0;
    if (playing && rate == 0) rate = 1; // the item's rate may lag behind the state
    double position = [p[@"elapsed"] doubleValue], at = [p[@"stamp"] doubleValue];
    if (at > 0 && rate > 0) position += (now - at) * rate;
    NSMutableDictionary *state = [p mutableCopy];
    [state removeObjectsForKeys:@[ @"elapsed", @"stamp", @"rate", @"playing" ]];
    state[@"state"] = ![p[@"title"] length] ? @"stopped" : playing ? @"playing" : @"paused";
    state[@"active"] = @([p[@"bundle"] isEqual:active]);
    [states addObject:[state copy]];
    NSArray *before = moving[p[@"bundle"]];
    if (before && fabs(position - ([before[0] doubleValue] + (now - [before[1] doubleValue]) * [before[2] doubleValue])) >= 1.5)
      jumped = YES;
    nowMoving[p[@"bundle"]] = @[ @(position), @(now), @(rate) ];
    state[@"position"] = @(position);
    [list addObject:state];
  }
  if ([states isEqual:last] && !jumped) return;
  last = states;
  moving = nowMoving;
  emit(@{@"players" : list});
  for (NSDictionary *p in list) {
    NSString *bundle = p[@"bundle"], *artwork = p[@"artwork"];
    if (!artwork.length || [artwork isEqual:covers[bundle]] || !players[bundle]) continue;
    // RustyBar has Music's and Spotify's covers from the apps themselves
    if ([bundle isEqual:@"com.apple.Music"] || [bundle isEqual:@"com.spotify.client"]) continue;
    covers[bundle] = artwork;
    requestCover(bundle, artwork, players[bundle]);
  }
}

static void update(void) {
  pending = NO;
  unsigned long update = ++updates;
  getClients(dispatch_get_main_queue(), ^(NSArray *clients) {
    NSMutableArray *asked = [NSMutableArray array];
    NSMutableDictionary *players = [NSMutableDictionary dictionary];
    dispatch_group_t group = dispatch_group_create();
    for (id client in [clients isKindOfClass:NSArray.class] ? clients : @[]) {
      NSString *bundle = bundleOf(client);
      if (!bundle.length || players[bundle]) continue;
      id path = ((id (*)(id, SEL, id, id, id))objc_msgSend)([PlayerPath alloc], @selector(initWithOrigin:client:player:), origin, client, nil);
      players[bundle] = path;
      [asked addObject:ask(client, path, group)];
    }
    // Sent when all have answered, or after a second with those that did
    __block BOOL sent = NO;
    Done send = ^{
      if (sent) return;
      sent = YES;
      report(asked, players, update);
    };
    dispatch_group_notify(group, dispatch_get_main_queue(), send);
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC), dispatch_get_main_queue(), send);
  });
}

// Many notifications come together: one update for them all
static void schedule(void) {
  if (pending) return;
  pending = YES;
  dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{
    @autoreleasepool {
      update();
    }
  });
}

static void command(NSString *line) {
  NSArray *words = [line componentsSeparatedByString:@" "];
  id path = words.count > 1 && [words[1] isEqual:activeBundle()] ? paths[words[1]] : nil;
  if (!path) return;
  int command = -1;
  NSDictionary *options = nil;
  if ([words[0] isEqual:@"toggle"]) command = TOGGLE;
  else if ([words[0] isEqual:@"next"]) command = NEXT;
  else if ([words[0] isEqual:@"previous"]) command = PREVIOUS;
  else if ([words[0] isEqual:@"seek"] && words.count > 2) {
    command = SEEK;
    options = @{positionOption : @([words[2] doubleValue])};
  }
  if (command < 0) return;
  sendCommand(command, options, path, 0, dispatch_get_main_queue(), ^(id result) {
    schedule();
  });
}

// Called by perl, as an XSUB: its arguments (the interpreter, the CV) aren't needed.
__attribute__((visibility("default"))) void rustybar_now_playing(void *perl, void *cv) {
  @autoreleasepool {
    void *mr = dlopen("/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote", RTLD_NOW);
    void (*registerForNotifications)(dispatch_queue_t) = mr ? dlsym(mr, "MRMediaRemoteRegisterForNowPlayingNotifications") : NULL;
    getClients = mr ? dlsym(mr, "MRMediaRemoteGetNowPlayingClients") : NULL;
    getState = mr ? dlsym(mr, "MRMediaRemoteGetPlaybackStateForPlayer") : NULL;
    getCommands = mr ? dlsym(mr, "MRMediaRemoteGetSupportedCommandsForPlayer") : NULL;
    requestQueue = mr ? dlsym(mr, "MRMediaRemoteRequestNowPlayingPlaybackQueueForPlayer") : NULL;
    sendCommand = mr ? dlsym(mr, "MRMediaRemoteSendCommandToPlayer") : NULL;
    CFStringRef *option = mr ? dlsym(mr, "kMRMediaRemoteOptionPlaybackPosition") : NULL;
    positionOption = option ? (__bridge NSString *)*option : nil;
    PlayerPath = NSClassFromString(@"MRPlayerPath");
    QueueRequest = NSClassFromString(@"MRPlaybackQueueRequest");
    NowPlaying = NSClassFromString(@"MRNowPlayingRequest");
    if (!registerForNotifications || !getClients || !getState || !getCommands || !requestQueue || !sendCommand || !positionOption ||
        !PlayerPath || !QueueRequest || ![NowPlaying respondsToSelector:@selector(localNowPlayingPlayerPath)]) {
      emit(@{@"error" : @"MediaRemote isn't what this helper knows"});
      exit(1);
    }
    Class Origin = NSClassFromString(@"MROrigin");
    origin = [Origin respondsToSelector:@selector(localOrigin)] ? ((id (*)(id, SEL))objc_msgSend)(Origin, @selector(localOrigin)) : nil;
    if (!origin) {
      emit(@{@"error" : @"MediaRemote has no origin for this Mac"});
      exit(1);
    }
    covers = [NSMutableDictionary dictionary];
    registerForNotifications(dispatch_get_main_queue());
    [NSNotificationCenter.defaultCenter addObserverForName:nil object:nil queue:nil usingBlock:^(NSNotification *n) {
      // A change of track posts six of them, of state a few: all MediaRemote's, MR… and kMR…
      if ([n.name hasPrefix:@"MR"] || [n.name hasPrefix:@"kMR"]) schedule();
    }];
    [NSThread detachNewThreadWithBlock:^{
      char *buffer = NULL;
      size_t size = 0;
      ssize_t n;
      while ((n = getline(&buffer, &size, stdin)) > 0) {
        NSString *line = [[[NSString alloc] initWithBytes:buffer length:n encoding:NSUTF8StringEncoding]
            stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceAndNewlineCharacterSet];
        if (line) dispatch_async(dispatch_get_main_queue(), ^{ command(line); });
      }
      exit(0);
    }];
    update();
  }
  CFRunLoopRun();
  exit(0);
}

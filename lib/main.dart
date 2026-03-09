import 'dart:io';
import 'dart:math';

import 'package:arcane/arcane.dart';
import 'package:auram_launcher/pack.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';
import 'package:microshaft/microshaft.dart';
import 'package:microshaft/src/model/shafted.dart';
import 'package:patterns_canvas/patterns_canvas.dart';
import 'package:url_launcher/url_launcher.dart';

void main() => runApp("auram_launcher", AuramLauncher());

class AuramLauncher extends StatelessWidget {
  const AuramLauncher({super.key});

  @override
  Widget build(BuildContext context) => ArcaneApp(
    home: MainScreen(),
    title: "Auram",
    theme: ArcaneTheme(
      themeMode: ThemeMode.dark,
      scheme: ContrastedColorScheme.fromScheme(ColorSchemes.violet),
    ),
  );
}

class MainScreen extends StatefulWidget {
  const MainScreen({super.key});

  @override
  State<MainScreen> createState() => _MainScreenState();
}

class AuramShaftStorage extends MicroshaftStorage {
  @override
  bool containsKey(String key) => hotBox.containsKey(key);

  @override
  Future<void> flush() => hotBox.flush();

  @override
  String? get(String key, [String? or]) => hotBox.get(key, defaultValue: or);

  @override
  List<String> keys() => hotBox.keys.whereType<String>().toList();

  @override
  void remove(String key) => hotBox.delete(key);

  @override
  void set(String key, String value) => hotBox.put(key, value);
}

class _MainScreenState extends State<MainScreen> {
  late PackInstance packInstance;
  late Future<void> initFuture;

  @override
  void initState() {
    super.initState();
    packInstance = PackInstance();
    initFuture = packInstance.initialize();
  }

  Future<Shafted> getShafted() => MicroshaftClient(storage: AuramShaftStorage())
      .authenticate((url, code) async {
        warningAnnounce("CODE: $code");
        await Clipboard.setData(ClipboardData(text: code));
        TextToast("Copied code '$code' to clipboard!").open(context);
        launchUrl(Uri.parse(url));
      })
      .thenRun((value) {
        successAnnounce("Logged in as ${value.profileName}");
      });

  Future<void> simulateLoading() async {
    while (true) {
      for (int i = 0; i <= 100; i++) {
        await Future.delayed(
          Duration(milliseconds: (500 * Random().nextDouble()).round()),
        );
        packInstance.progressStream.add(("Loading... $i%", i / 100));
      }

      packInstance.progressStream.add(("Launching", -1));
      await Future.delayed(Duration(seconds: 3));
    }
  }

  Future<void> launch() async {
    await initFuture;
    packInstance.progressStream.add(("Launching", -1));
    verboseAnnounce("Launching Auram");
    Shafted auth = await getShafted();
    await packInstance.launch(auth);
    exit(0);
  }

  Future<void> openDataFolder() async {
    try {
      await initFuture;
      await packInstance.openDataFolder();
    } on Object catch (error) {
      if (!mounted) return;
      TextToast("Failed to open data folder: $error").open(context);
    }
  }

  Future<void> forceReinstall() async {
    try {
      await initFuture;
      await packInstance.forceReinstall();
    } on Object catch (error) {
      if (!mounted) return;
      TextToast("Force reinstall failed: $error").open(context);
    }
  }

  Future<void> buildServer() async {
    try {
      await initFuture;
      await packInstance.buildServer();
    } on Object catch (error) {
      if (!mounted) return;
      TextToast("Build server failed: $error").open(context);
    }
  }

  @override
  void dispose() {
    packInstance.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Screen(
    gutter: false,
    overrideBackgroundColor: Colors.black,
    child: Stack(
      children: [
        Transform.scale(
          scale: 2,
          child:
              CustomPaint(
                    painter: AuramBackgroundPainter(
                      w: MediaQuery.of(context).size.width ~/ 16,
                      bg: Theme.of(context).colorScheme.background,
                      fg: Theme.of(
                        context,
                      ).colorScheme.primary.withOpacity(0.4),
                    ),
                    child: SizedBox(
                      child: Row(children: [Column(children: [])]),
                    ),
                  )
                  .shadeWarpAnimation(
                    frequency: 1,
                    amplitude: 10,
                    zSpeed: 0.025,
                  )
                  .shadeWarpAnimation(
                    frequency: 3,
                    amplitude: 50,
                    zSpeed: 0.01,
                    z: 500,
                  )
                  .shadeWarpAnimation(
                    frequency: 0.5,
                    amplitude: 150,
                    zSpeed: 0.04,
                    z: 100,
                  )
                  .shadeRGB(radius: 3)
                  .shadeWarpAnimation(
                    frequency: 4,
                    amplitude: 80,
                    zSpeed: 0.05,
                    z: 10000,
                  )
                  .shadeEdge(0.0001),
        ),
        Center(
          child: packInstance.progressStream.buildNullable(
            (p) => p == null
                ? Stack(
                    alignment: Alignment.center,
                    children: [
                      Row(
                        mainAxisSize: .min,
                        children: [
                          PrimaryButton(onPressed: launch, child: Text("Play")),
                          Gap(8),
                          IconButtonMenu(
                            icon: Icons.gear_six_fill,
                            items: [
                              MenuButton(
                                leading: Icon(Icons.folder),
                                onPressed: () {
                                  openDataFolder();
                                },
                                child: Text("Data Folder"),
                              ),
                              MenuButton(
                                leading: Icon(Icons.triangle),
                                onPressed: () => DialogConfirm(
                                  title: "Are you sure?",
                                  description:
                                      "This will also delete your settings, saved worlds, resource packs, everything!",
                                  destructive: true,
                                  confirmText: "Force Reinstall",
                                  onConfirm: () {
                                    forceReinstall();
                                  },
                                ).open(context),
                                child: Text("Force Reinstall"),
                              ),
                              MenuButton(
                                leading: Icon(Icons.server_outline_ionic),
                                onPressed: () {
                                  buildServer();
                                },
                                child: Text("Build Server"),
                              ),
                            ],
                          ),
                        ],
                      ),
                      Align(
                        alignment: Alignment.bottomCenter,
                        child: Text(
                          "v${packInstance.currentPackTag ?? "unknown"}",
                        ).muted.xSmall.pad(8),
                      ),
                    ],
                  )
                : Stack(
                    alignment: Alignment.center,
                    children: [
                      OverflowBox(
                        alignment: Alignment.center,
                        child: Center(
                          child: ProgressRing(value: p.$2 == -1 ? null : p.$2),
                        ),
                      ),
                      Align(
                        alignment: Alignment.bottomCenter,
                        child: Column(
                          crossAxisAlignment: .start,
                          mainAxisSize: .min,
                          children: [
                            Text(p.$1).muted.xSmall.padLeft(8),
                            Row(
                              children: [
                                Expanded(
                                  child: LinearProgressIndicator(
                                    value: p.$2 == -1 ? null : p.$2,
                                  ),
                                ),
                              ],
                            ),
                          ],
                        ),
                      ).padBottom(1),
                    ],
                  ),
          ),
        ),
      ],
    ),
  );
}

class ProgressRing extends StatefulWidget {
  final double? value;
  final double chase;
  final double snapEpsilon;
  final Curve chaseCurve;
  const ProgressRing({
    super.key,
    required this.value,
    this.chase = 0.1,
    this.snapEpsilon = 0.0005,
    this.chaseCurve = Curves.easeOutCubic,
  });

  @override
  State<ProgressRing> createState() => _ProgressRingState();
}

class _ProgressRingState extends State<ProgressRing>
    with SingleTickerProviderStateMixin {
  double v = 0;
  double t = 0;
  late Ticker ticker;

  double _chaseTo({
    required double current,
    required double target,
    required double normalizer,
  }) {
    double delta = target - current;
    double distance = delta.abs();
    if (distance <= widget.snapEpsilon) return target;
    double scale = normalizer <= 0 ? 1 : normalizer;
    double normalized = (distance / scale).clamp(0.0, 1.0).toDouble();
    double curved = widget.chaseCurve.transform(normalized);
    double step = (distance * widget.chase * curved).clamp(
      widget.snapEpsilon,
      distance,
    );
    return current + (delta < 0 ? -step : step);
  }

  @override
  void initState() {
    super.initState();
    ticker = createTicker((dur) {
      setState(() {
        double targetValue = widget.value ?? -1;
        if (targetValue > 1) targetValue = 1;
        if (targetValue < -1) targetValue = -1;
        double targetThickness = targetValue < 0 ? 10 : (40 * targetValue);
        v = _chaseTo(current: v, target: targetValue, normalizer: 2);
        t = _chaseTo(current: t, target: targetThickness, normalizer: 40);
      });
    });
    ticker.start();
  }

  @override
  void dispose() {
    ticker.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Stack(
    alignment: Alignment.center,
    children: [
      if (true)
        CascadeEffect.prism(
          color: Colors.black,
          repeats: 12,
          shimmer: 10 * v.abs(),
          thickness: 0.1 + (0.7 * v.abs()),
          size: 500,
        ),
    ],
  );
}

class AuramBackgroundPainter extends CustomPainter {
  final Color fg;
  final Color bg;
  final int w;

  const AuramBackgroundPainter({
    required this.fg,
    required this.bg,
    required this.w,
  });

  @override
  void paint(Canvas canvas, Size size) {
    Raindrops(
      bgColor: bg,
      fgColor: fg,
      featuresCount: w,
    ).paintOnWidget(canvas, size);
  }

  @override
  bool shouldRepaint(CustomPainter oldDelegate) => true;
}

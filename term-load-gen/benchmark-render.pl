#!/usr/bin/env perl
# Benchmark Script: Compare Software vs Cairo2D rendering backends
# Usage: ./benchmark-render.pl [--runs N] [--duration D] [--quick]

use strict;
use warnings;
use Getopt::Long;
use POSIX qw(strftime);

# Configuration
my $runs = 3;
my $duration = 10;
my $quick = 0;
use FindBin qw($RealBin);
use Cwd qw(abs_path);
my $base_dir = abs_path("$RealBin/..");  # wezterm root relative to script location
my $wezterm_bin = $ENV{WEZTERM_BIN} // "$base_dir/target/release/wezterm-gui";
my $term_load_gen = "$base_dir/term-load-gen/target/release/term-load-gen";

GetOptions(
    'runs=i'     => \$runs,
    'duration=i' => \$duration,
    'quick'      => \$quick,
    'wezterm=s'  => \$wezterm_bin,
    'help'       => sub { usage(); exit 0; },
) or do { usage(); exit 1; };

sub usage {
    print <<EOF;
Usage: $0 [options]

Options:
  --runs N       Number of runs per scenario (default: 3)
  --duration D   Test duration in seconds (default: 10)
  --quick        Run minimal set of scenarios
  --wezterm PATH Path to wezterm binary (or set WEZTERM_BIN env)
  --help         Show this help

Environment:
  WEZTERM_BIN    Path to wezterm binary (default: wezterm)
EOF
}

my @backends = ('Software', 'Cairo2D');

my @scenarios = (
    { name => 'idle',        args => '--idle --prefill',                           desc => 'Idle (prefilled)' },
    { name => 'typing',      args => '-r 30 --vary chars --prefill -p sweep',       desc => 'Typing (1 char @30Hz)' },
    { name => 'single_cell', args => '-r 120 --vary chars --prefill',              desc => 'Single cell @120Hz' },
    { name => 'line_sweep',  args => '-w full -p sweep -r 60 --vary chars',        desc => 'Full line sweep' },
    { name => 'full_screen', args => '-w full -H full -r 30 --vary chars',         desc => 'Full screen @30Hz' },
    { name => 'scroll',      args => '--scroll -r 60 --vary chars',                desc => 'Scrolling output' },
    { name => 'vary_both',   args => '-w 20 --vary both -r 60',                    desc => 'Varying chars+colors' },
    { name => 'random',      args => '-w 10 -H 5 -p random -r 60 --vary chars',    desc => 'Random positions' },
);

if ($quick) {
    @scenarios = grep { $_->{name} =~ /^(idle|typing|single_cell|scroll|full_screen)$/ } @scenarios;
}

# Results storage
my %results;  # {backend}{scenario}[run] = { real, user, sys, maxrss, stats }

# Check prerequisites
sub check_prerequisites {
    unless (-x $term_load_gen) {
        die "Error: $term_load_gen not found.\n" .
            "Build with: cargo build --release -p term-load-gen\n";
    }

    my $version = `$wezterm_bin --version 2>&1`;
    if ($?) {
        die "Error: Cannot run $wezterm_bin\n";
    }
    print "Using: $version";
}

# Parse /usr/bin/time -v output
sub parse_time_output {
    my ($output) = @_;
    my %data;

    # User time (seconds): 0.85
    if ($output =~ /User time \(seconds\):\s*([\d.]+)/) {
        $data{user} = $1;
    }
    # System time (seconds): 0.12
    if ($output =~ /System time \(seconds\):\s*([\d.]+)/) {
        $data{sys} = $1;
    }
    # Elapsed (wall clock) time (h:mm:ss or m:ss): 0:10.05
    if ($output =~ /Elapsed.*?:\s*(?:(\d+):)?(\d+):([\d.]+)/) {
        my $h = $1 // 0;
        my $m = $2;
        my $s = $3;
        $data{real} = $h * 3600 + $m * 60 + $s;
    }
    # Maximum resident set size (kbytes): 245000
    if ($output =~ /Maximum resident set size.*?:\s*(\d+)/) {
        $data{maxrss} = $1 / 1024;  # Convert to MB
    }
    # Percent of CPU: 9%
    if ($output =~ /Percent of CPU.*?:\s*(\d+)/) {
        $data{cpu_pct} = $1;
    }

    return \%data;
}

# Parse wezterm stats from stderr
sub parse_wezterm_stats {
    my ($output) = @_;
    my %stats;

    # Parse throughput table (rates)
    # Format: Key(stat.name)  current  p50  p75  p95
    while ($output =~ /^Key\(([\w.]+)\)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)/mg) {
        my ($name, $current, $p50, $p75, $p95) = ($1, $2, $3, $4, $5);
        $stats{$name} = { current => $current, p50 => $p50, p75 => $p75, p95 => $p95 };
    }

    # Parse latency table
    # Format: Key(stat.name)  p50  p75  p95 (with duration like 1.23ms or 123.45µs)
    while ($output =~ /^Key\(([\w.]+)\)\s+([\d.]+[µmn]?s)\s+([\d.]+[µmn]?s)\s+([\d.]+[µmn]?s)/mg) {
        my ($name, $p50, $p75, $p95) = ($1, $2, $3, $4);
        next if exists $stats{$name};  # Don't overwrite rates
        $stats{$name} = { p50 => $p50, p75 => $p75, p95 => $p95 };
    }

    # Parse counters
    # Format: Key(stat.name)  COUNT
    while ($output =~ /^Key\(([\w.]+)\)\s+(\d+)\s*$/mg) {
        my ($name, $count) = ($1, $2);
        next if exists $stats{$name};  # Don't overwrite if already parsed
        $stats{$name} = { count => $count };
    }

    # Parse gauges
    # Format: Key(stat.name)  VALUE (float)
    while ($output =~ /^Key\(([\w.]+)\)\s+([\d.]+)\s*$/mg) {
        my ($name, $value) = ($1, $2);
        next if exists $stats{$name};
        $stats{$name} = { value => $value };
    }

    return \%stats;
}

# Run a single benchmark
sub run_benchmark {
    my ($backend, $scenario) = @_;

    my $args = "$scenario->{args} -d $duration -q";

    # Build command - --config must come before 'start' subcommand
    # Pass terminal size explicitly since detection may not work inside wezterm
    my $cmd_str = sprintf(
        q{/usr/bin/time -v %s --config 'front_end="%s"' --config 'periodic_stat_logging=2' start -- %s %s --term-width 120 --term-height 40},
        $wezterm_bin, $backend, $term_load_gen, $args
    );

    # Run and capture all output (time outputs to stderr)
    my $output = `$cmd_str 2>&1`;
    my $exit_code = $?;

    if ($exit_code != 0) {
        warn "Warning: Command exited with code $exit_code\n";
        warn "Command: $cmd_str\n";
    }

    my $time_data = parse_time_output($output);
    my $stats = parse_wezterm_stats($output);

    return {
        %$time_data,
        stats => $stats,
        output => $output,
    };
}

# Calculate statistics
sub calc_stats {
    my (@values) = @_;
    return (0, 0) unless @values;

    my $n = scalar @values;
    my $sum = 0;
    $sum += $_ for @values;
    my $mean = $sum / $n;

    my $variance = 0;
    $variance += ($_ - $mean) ** 2 for @values;
    my $stddev = $n > 1 ? sqrt($variance / ($n - 1)) : 0;

    return ($mean, $stddev);
}

# Main benchmark loop
sub run_benchmarks {
    print "\n=== Running Benchmarks ===\n";
    print "Backends: ", join(", ", @backends), "\n";
    print "Scenarios: ", scalar(@scenarios), "\n";
    print "Runs per test: $runs\n";
    print "Duration per run: ${duration}s\n\n";

    my $total = scalar(@backends) * scalar(@scenarios) * $runs;
    my $current = 0;

    for my $backend (@backends) {
        for my $scenario (@scenarios) {
            print "[$backend] $scenario->{desc}";
            $| = 1;  # Flush output

            for my $run (1 .. $runs) {
                $current++;
                print " .";

                my $result = run_benchmark($backend, $scenario);
                push @{$results{$backend}{$scenario->{name}}}, $result;
            }
            print " done\n";
        }
    }
}

# Generate report
sub generate_report {
    my $date = strftime("%Y-%m-%d %H:%M", localtime);

    print "\n";
    print "=" x 70, "\n";
    print "Rendering Backend Benchmark Report\n";
    print "=" x 70, "\n";
    print "Date: $date\n";
    print "Runs per test: $runs | Duration: ${duration}s\n\n";

    # CPU Time Comparison table
    print "CPU Time Comparison:\n";
    printf "%-20s | %-8s | %7s | %7s | %7s | %5s | %8s\n",
           "Scenario", "Backend", "Real(s)", "User(s)", "Sys(s)", "CPU%", "RSS(MB)";
    print "-" x 20, "-+-", "-" x 8, "-+-", "-" x 7, "-+-", "-" x 7, "-+-",
          "-" x 7, "-+-", "-" x 5, "-+-", "-" x 8, "\n";

    my %cpu_totals;
    my %rss_totals;
    my %improvements;

    for my $scenario (@scenarios) {
        for my $backend (@backends) {
            my $runs_data = $results{$backend}{$scenario->{name}} // [];

            my @reals = map { $_->{real} // 0 } @$runs_data;
            my @users = map { $_->{user} // 0 } @$runs_data;
            my @syss  = map { $_->{sys} // 0 } @$runs_data;
            my @rsss  = map { $_->{maxrss} // 0 } @$runs_data;

            my ($real_mean, $real_std) = calc_stats(@reals);
            my ($user_mean, $user_std) = calc_stats(@users);
            my ($sys_mean, $sys_std)   = calc_stats(@syss);
            my ($rss_mean, $rss_std)   = calc_stats(@rsss);

            my $cpu_pct = $real_mean > 0 ? 100 * ($user_mean + $sys_mean) / $real_mean : 0;

            printf "%-20s | %-8s | %7.2f | %7.2f | %7.2f | %4.0f%% | %8.0f\n",
                   $scenario->{desc}, $backend, $real_mean, $user_mean, $sys_mean, $cpu_pct, $rss_mean;

            $cpu_totals{$backend}{$scenario->{name}} = $user_mean + $sys_mean;
            $rss_totals{$backend}{$scenario->{name}} = $rss_mean;
        }
    }

    # Calculate improvements
    print "\n";
    print "Cairo2D vs Software Improvement:\n";
    printf "%-20s | %10s | %10s\n", "Scenario", "CPU Saved", "RAM Saved";
    print "-" x 20, "-+-", "-" x 10, "-+-", "-" x 10, "\n";

    my @cpu_savings;
    my @rss_savings;

    for my $scenario (@scenarios) {
        my $sw_cpu = $cpu_totals{Software}{$scenario->{name}} // 0;
        my $c2_cpu = $cpu_totals{Cairo2D}{$scenario->{name}} // 0;
        my $sw_rss = $rss_totals{Software}{$scenario->{name}} // 0;
        my $c2_rss = $rss_totals{Cairo2D}{$scenario->{name}} // 0;

        my $cpu_save = $sw_cpu > 0 ? 100 * ($sw_cpu - $c2_cpu) / $sw_cpu : 0;
        my $rss_save = $sw_rss > 0 ? 100 * ($sw_rss - $c2_rss) / $sw_rss : 0;

        push @cpu_savings, $cpu_save;
        push @rss_savings, $rss_save;

        $improvements{$scenario->{name}} = { cpu => $cpu_save, rss => $rss_save };

        printf "%-20s | %+9.1f%% | %+9.1f%%\n",
               $scenario->{desc}, $cpu_save, $rss_save;
    }

    # Cairo2D-specific stats (from last run)
    print "\n";
    print "Cairo2D Internal Stats (last run):\n";
    printf "%-20s | %10s | %10s | %12s | %10s\n",
           "Scenario", "GlyphHit%", "FrameSkip%", "BytesSaved", "Paint(p50)";
    print "-" x 20, "-+-", "-" x 10, "-+-", "-" x 10, "-+-", "-" x 12, "-+-", "-" x 10, "\n";

    for my $scenario (@scenarios) {
        my $runs_data = $results{Cairo2D}{$scenario->{name}} // [];
        my $last_run = $runs_data->[-1] // {};
        my $stats = $last_run->{stats} // {};

        # Calculate glyph cache hit rate (using counter counts)
        my $cache_hit = $stats->{'cairo2d.glyph_cache.hit'}{count} // 0;
        my $cache_miss = $stats->{'cairo2d.glyph_cache.miss'}{count} // 0;
        my $cache_pct = ($cache_hit + $cache_miss) > 0
            ? sprintf("%.1f%%", 100 * $cache_hit / ($cache_hit + $cache_miss))
            : '-';

        # Frame skip percentage
        my $frame_skip = $stats->{'cairo2d.frame_area_update_skip_1s_pct'}{value} // '-';
        $frame_skip = sprintf("%.1f%%", $frame_skip) if $frame_skip ne '-';

        # Bytes saved ratio
        my $bytes_sent = $stats->{'cairo2d.partial.bytes_sent'}{count} // 0;
        my $bytes_saved = $stats->{'cairo2d.partial.bytes_saved'}{count} // 0;
        my $bytes_ratio = ($bytes_sent + $bytes_saved) > 0
            ? sprintf("%.1f%%", 100 * $bytes_saved / ($bytes_sent + $bytes_saved))
            : '-';

        # Paint latency
        my $paint = $stats->{'gui.paint.impl'}{p50} // '-';

        printf "%-20s | %10s | %10s | %12s | %10s\n",
               $scenario->{desc}, $cache_pct, $frame_skip, $bytes_ratio, $paint;
    }

    # Summary
    print "\n";
    print "=" x 70, "\n";
    print "Summary:\n";

    my ($avg_cpu, undef) = calc_stats(@cpu_savings);
    my ($avg_rss, undef) = calc_stats(@rss_savings);

    printf "- Cairo2D uses %.1f%% %s CPU than Software on average\n",
           abs($avg_cpu), $avg_cpu >= 0 ? "less" : "more";
    printf "- Cairo2D uses %.1f%% %s memory than Software on average\n",
           abs($avg_rss), $avg_rss >= 0 ? "less" : "more";

    # Find best and worst
    my @sorted = sort { $improvements{$b}{cpu} <=> $improvements{$a}{cpu} } keys %improvements;
    if (@sorted) {
        my $best = $sorted[0];
        my $worst = $sorted[-1];
        my $best_scenario = (grep { $_->{name} eq $best } @scenarios)[0];
        my $worst_scenario = (grep { $_->{name} eq $worst } @scenarios)[0];

        printf "- Best improvement: %s (%.1f%% CPU reduction)\n",
               $best_scenario->{desc}, $improvements{$best}{cpu};
        printf "- Smallest improvement: %s (%.1f%% CPU %s)\n",
               $worst_scenario->{desc}, abs($improvements{$worst}{cpu}),
               $improvements{$worst}{cpu} >= 0 ? "reduction" : "increase";
    }
    print "=" x 70, "\n";
}

# Main
check_prerequisites();
run_benchmarks();
generate_report();

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Dataset, Paragraph},
    Frame, Terminal,
};
use std::{
    io::{self, BufRead, BufReader},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// Represents a single ping measurement
struct PingSample {
    rtt: u32,
    timestamp: Instant,
}

/// The main application state and logic
struct PingApp {
    rtts: Vec<PingSample>,
    ping_results: Vec<String>,
    total_sent: u32,
    total_received: u32,
    rx: mpsc::Receiver<String>,
}

impl PingApp {
    /// Create a new instance of the application
    fn new(rx: mpsc::Receiver<String>) -> Self {
        Self {
            rtts: Vec::new(),
            ping_results: Vec::new(),
            total_sent: 0,
            total_received: 0,
            rx,
        }
    }

    /// Process incoming ping data from the channel
    fn update(&mut self) {
        while let Ok(line) = self.rx.try_recv() {
            let mut sample_pushed = false;
            
            // Parse RTT if present
            if let Some(time_part) = line.split_whitespace().find(|s| s.starts_with("time=")) {
                let time_str = time_part.replace("time=", "").replace("ms", "");
                if let Ok(time) = time_str.parse::<u32>() {
                    self.rtts.push(PingSample {
                        rtt: time,
                        timestamp: Instant::now(),
                    });
                    self.total_received += 1;
                    self.total_sent += 1;
                    sample_pushed = true;
                }
            }
            
            // Handle non-RTT lines (timeouts, errors) as gaps
            if !sample_pushed && !line.trim().is_empty() && !line.starts_with("Pinging") {
                self.rtts.push(PingSample {
                    rtt: 0,
                    timestamp: Instant::now(),
                });
                self.total_sent += 1;
            }
            
            // Store raw logs
            self.ping_results.push(line);
            if self.ping_results.len() > 100 {
                self.ping_results.remove(0);
            }
        }
    }

    /// Draw the entire UI
    fn draw(&self, f: &mut Frame) {
        let size = f.area();

        // 2x2 Grid Layout
        let vertical_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(size);

        let top_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(vertical_chunks[0]);

        let bottom_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(vertical_chunks[1]);

        // Render each quadrant
        self.render_ping_logs(f, top_chunks[0]);
        self.render_statistics(f, top_chunks[1]);
        self.render_info_hub(f, bottom_chunks[0]);
        self.render_rtt_chart(f, bottom_chunks[1]);
    }

    /// Top Left: Raw ping output logs
    fn render_ping_logs(&self, f: &mut Frame, area: Rect) {
        let height = area.height.saturating_sub(2) as usize;
        let display_lines = if self.ping_results.len() > height {
            &self.ping_results[self.ping_results.len() - height..]
        } else {
            &self.ping_results
        };
        
        let paragraph = Paragraph::new(display_lines.join("\n"))
            .block(Block::default().title("Ping Results").borders(Borders::ALL));
        f.render_widget(paragraph, area);
    }

    /// Top Right: Aggregated metrics
    fn render_statistics(&self, f: &mut Frame, area: Rect) {
        let min_ping = self.rtts.iter().filter(|s| s.rtt > 0).map(|s| s.rtt).min().unwrap_or(0);
        let max_ping = self.rtts.iter().map(|s| s.rtt).max().unwrap_or(0);
        let valid_samples: Vec<_> = self.rtts.iter().filter(|s| s.rtt > 0).collect();
        let avg_ping = if !valid_samples.is_empty() {
            valid_samples.iter().map(|s| s.rtt).sum::<u32>() / valid_samples.len() as u32
        } else {
            0
        };
        
        let loss = if self.total_sent > 0 {
            (self.total_sent - self.total_received) as f64 / self.total_sent as f64 * 100.0
        } else {
            0.0
        };

        let stats_content = format!(
            "Total Pings: {}\n\
             Received:    {}\n\
             Lost:        {} ({:.1}%)\n\n\
             Min RTT:     {}ms\n\
             Max RTT:     {}ms\n\
             Avg RTT:     {}ms",
            self.total_sent, self.total_received, self.total_sent - self.total_received, loss, min_ping, max_ping, avg_ping
        );
        
        let paragraph = Paragraph::new(stats_content)
            .block(Block::default().title("Ping Statistics").borders(Borders::ALL));
        f.render_widget(paragraph, area);
    }

    /// Bottom Left: Branding and Help
    fn render_info_hub(&self, f: &mut Frame, area: Rect) {
        let logo = vec![
            " ____   ___  _   _  ____ ",
            "|  _ \\ / _ \\| \\ | |/ ___|",
            "| |_) | | | |  \\| | |  _ ",
            "|  __/| |_| | |\\  | |_| |",
            "|_|    \\___/|_| \\_|\\____|",
        ];
        
        let mut info_text = Vec::new();
        for line in logo {
            info_text.push(Line::from(Span::styled(line, Style::default().fg(Color::Cyan))));
        }
        info_text.push(Line::from(""));
        info_text.push(Line::from(Span::styled("Author: Sonic515", Style::default().fg(Color::Yellow))));
        info_text.push(Line::from(""));
        info_text.push(Line::from(Span::styled("press 'q' for exit", Style::default().fg(Color::DarkGray))));

        let paragraph = Paragraph::new(info_text)
            .block(Block::default().title("Info").borders(Borders::ALL))
            .alignment(ratatui::layout::Alignment::Center);
        f.render_widget(paragraph, area);
    }

    /// Bottom Right: High-fidelity animated chart
    fn render_rtt_chart(&self, f: &mut Frame, area: Rect) {
        let max_samples = 100;
        let samples = if self.rtts.len() > max_samples {
            &self.rtts[self.rtts.len() - max_samples..]
        } else {
            &self.rtts
        };
        
        let now = Instant::now();
        let scroll_fraction = if let Some(last) = samples.last() {
            let elapsed = now.duration_since(last.timestamp);
            (elapsed.as_secs_f64() * 1.0).min(1.0)
        } else {
            0.0
        };
        
        let base_offset = max_samples as f64 - samples.len() as f64 - scroll_fraction;
        
        let mut green_data = Vec::new();
        let mut yellow_data = Vec::new();
        let mut red_data = Vec::new();
        
        for (i, sample) in samples.iter().enumerate() {
            let x_pos = base_offset + i as f64;
            let rtt_val = sample.rtt as f64;
            
            if sample.rtt > 0 {
                let target_vec = if sample.rtt < 60 {
                    &mut green_data
                } else if sample.rtt < 150 {
                    &mut yellow_data
                } else {
                    &mut red_data
                };
                
                target_vec.push((x_pos, 0.0));
                target_vec.push((x_pos, rtt_val));
                target_vec.push((x_pos + 1.0, rtt_val));
                target_vec.push((x_pos + 1.0, 0.0));
            }
        }

        let avg_rtt_val = if !samples.is_empty() {
            samples.iter().map(|s| s.rtt).sum::<u32>() as f64 / samples.len() as f64
        } else {
            0.0
        };
        let max_rtt_val = samples.iter().map(|s| s.rtt).max().unwrap_or(0) as f64;
        
        let avg_line_data = vec![(0.0, avg_rtt_val), (100.0, avg_rtt_val)];
        let max_line_data = vec![(0.0, max_rtt_val), (100.0, max_rtt_val)];

        let datasets = vec![
            Dataset::default()
                .name("Low")
                .marker(symbols::Marker::Block)
                .graph_type(ratatui::widgets::GraphType::Line)
                .style(Style::default().fg(Color::Green))
                .data(&green_data),
            Dataset::default()
                .name("Med")
                .marker(symbols::Marker::Block)
                .graph_type(ratatui::widgets::GraphType::Line)
                .style(Style::default().fg(Color::Yellow))
                .data(&yellow_data),
            Dataset::default()
                .name("High")
                .marker(symbols::Marker::Block)
                .graph_type(ratatui::widgets::GraphType::Line)
                .style(Style::default().fg(Color::Red))
                .data(&red_data),
            Dataset::default()
                .name("Avg")
                .marker(symbols::Marker::Braille)
                .graph_type(ratatui::widgets::GraphType::Line)
                .style(Style::default().fg(Color::Cyan))
                .data(&avg_line_data),
            Dataset::default()
                .name("Max")
                .marker(symbols::Marker::Braille)
                .graph_type(ratatui::widgets::GraphType::Line)
                .style(Style::default().fg(Color::Magenta))
                .data(&max_line_data),
        ];

        let y_limit = (max_rtt_val * 1.1).max(50.0);
        let chart = Chart::new(datasets)
            .block(Block::default().title("RTT History").borders(Borders::ALL))
            .x_axis(Axis::default().style(Style::default().fg(Color::Gray)).bounds([0.0, 100.0]).labels(vec![Span::from("Oldest"), Span::from(""), Span::from("Newest")]))
            .y_axis(Axis::default().title("ms").style(Style::default().fg(Color::Gray)).bounds([0.0, y_limit]).labels(vec![
                Span::from("0"),
                Span::from(format!("{:.0}", y_limit * 0.25)),
                Span::from(format!("{:.0}", y_limit * 0.5)),
                Span::from(format!("{:.0}", y_limit * 0.75)),
                Span::from(format!("{:.0}", y_limit)),
            ]));
        f.render_widget(chart, area);
    }
}

fn main() -> Result<(), io::Error> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // If no arguments provided, show ping help and exit
    if args.is_empty() {
        let mut child = Command::new("ping")
            .spawn()
            .expect("Failed to execute ping help");
        let _ = child.wait();
        return Ok(());
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Channel for asynchronous ping communication
    let (tx, rx) = mpsc::channel();

    // Spawn the background worker thread for pinging
    let thread_args = args.clone();
    thread::spawn(move || {
        let mut child = Command::new("ping")
            .args(thread_args)
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to execute ping command");

        let stdout = child.stdout.take().expect("Failed to open stdout");
        let reader = BufReader::new(stdout);

        for line in reader.lines() {
            if let Ok(l) = line {
                if tx.send(l).is_err() { break; }
            }
        }
        let _ = child.kill();
    });

    // Initialize Application State
    let mut app = PingApp::new(rx);

    // Main Control Loop
    loop {
        app.update();

        terminal.draw(|f| app.draw(f))?;

        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') { break; }
            }
        }
    }

    // Graceful Shutdown
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
use crate::bcf::report::oncoprint::WriteErr;
use anyhow::Context as AnyhowContext;
use anyhow::Result;
use chrono::{DateTime, Local};
use derive_new::new;
use itertools::Itertools;
use lz_str::compress_to_utf16;
use serde_derive::Serialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::str::FromStr;
use tera::{Context, Tera};
use xlsxwriter::*;

type LookupTable = HashMap<String, HashMap<String, Vec<(String, usize, usize)>>>;

#[allow(clippy::too_many_arguments)]
pub(crate) fn csv_report(
    csv_path: &str,
    output_path: &str,
    rows_per_page: usize,
    separator: char,
    sort_column: Option<&str>,
    ascending: Option<bool>,
    formatter: Option<&str>,
    pin_until: Option<&str>,
) -> Result<()> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(separator as u8)
        .from_path(csv_path)?;

    let header = rdr.headers()?.clone();
    let titles = header.iter().collect_vec();
    let mut table = Vec::new();
    let mut numeric = HashMap::new();
    let mut non_numeric = HashMap::new();
    let mut integer = HashMap::new();
    for res in rdr.records() {
        let row = res?;
        let mut table_entry = HashMap::new();
        for (i, tile) in titles.iter().enumerate() {
            table_entry.insert(tile.to_string(), row[i].to_owned());
            match f32::from_str(&row[i]) {
                Ok(_) => {
                    let num = numeric.entry(tile.to_owned()).or_insert_with(|| 0);
                    *num += 1;
                    if i32::from_str(&row[i]).is_ok() {
                        let int = integer.entry(tile.to_owned()).or_insert_with(|| 0);
                        *int += 1;
                    }
                }
                _ => {
                    let no_num = non_numeric.entry(tile.to_owned()).or_insert_with(|| 0);
                    *no_num += 1;
                }
            }
        }
        table.push(table_entry);
    }

    let mut is_numeric = HashMap::new();
    for title in &titles {
        let is_num = match (numeric.get(title), non_numeric.get(title)) {
            (Some(num), Some(no_num)) => num > no_num,
            (Some(_), None) => true,
            _ => false,
        };
        is_numeric.insert(title.to_owned(), is_num);
    }

    let mut is_integer = HashMap::new();
    for title in &titles {
        let is_int = match (integer.get(title), non_numeric.get(title)) {
            (Some(num), Some(no_num)) => num > no_num,
            (Some(_), None) => true,
            _ => false,
        };
        is_integer.insert(title.to_owned(), is_int);
    }

    let mut plot_data = HashMap::new();
    let mut num_plot_data = HashMap::new();
    let mut reasonable_plot = titles.iter().map(|t| (*t, true)).collect::<HashMap<_, _>>();

    for title in &titles {
        match is_numeric.get(title) {
            Some(true) => {
                let plot = num_plot(&table, title.to_string());
                num_plot_data.insert(title, plot);
            }
            Some(false) => {
                if let Some(plot) = nominal_plot(&table, title.to_string()) {
                    plot_data.insert(title, plot);
                } else {
                    plot_data.insert(title, vec![]);
                    reasonable_plot.insert(title, false);
                }
            }
            _ => unreachable!(),
        };
    }

    match (sort_column, ascending) {
        (Some(column), Some(true)) => table.sort_by(|a, b| {
            match (
                f32::from_str(a.get(column).unwrap()),
                f32::from_str(b.get(column).unwrap()),
            ) {
                (Ok(float_a), Ok(float_b)) => float_a.partial_cmp(&float_b).unwrap(),
                _ => a.get(column).cmp(&b.get(column)),
            }
        }),
        (Some(column), Some(false)) => table.sort_by(|a, b| {
            match (
                f32::from_str(a.get(column).unwrap()),
                f32::from_str(b.get(column).unwrap()),
            ) {
                (Ok(float_a), Ok(float_b)) => float_b.partial_cmp(&float_a).unwrap(),
                _ => a.get(column).cmp(&b.get(column)),
            }
        }),
        (_, _) => {}
    }

    let wb = Workbook::new(&(output_path.to_owned() + "/report.xlsx"));
    let mut sheet = wb.add_worksheet(Some("Report"))?;
    for (i, title) in titles.iter().enumerate() {
        sheet.write_string(0, i.try_into()?, title, None)?;
    }

    for (i, row) in table.iter().enumerate() {
        for (c, title) in titles.iter().enumerate() {
            sheet.write_string(
                (i + 1).try_into()?,
                c.try_into()?,
                row.get(*title).unwrap(),
                None,
            )?;
        }
    }

    wb.close()?;

    let pages = if table.len() % rows_per_page == 0 && !table.is_empty() {
        (table.len() / rows_per_page) - 1
    } else {
        table.len() / rows_per_page
    };

    let plot_path = output_path.to_owned() + "/plots/";
    fs::create_dir(Path::new(&plot_path)).context(WriteErr::CantCreateDir {
        dir_path: plot_path.to_owned(),
    })?;

    for (n, title) in titles.iter().enumerate() {
        let mut templates = Tera::default();
        templates.add_raw_template("plot.js.tera", include_str!("plot.js.tera"))?;
        let mut context = Context::new();
        match is_numeric.get(title) {
            Some(true) => {
                context.insert(
                    "table",
                    &json!(num_plot_data.get(title).unwrap()).to_string(),
                );
                context.insert("num", &true);
            }
            Some(false) => {
                context.insert("table", &json!(plot_data.get(title).unwrap()).to_string());
                context.insert("num", &false);
            }
            _ => unreachable!(),
        }
        context.insert("title", &title);
        context.insert("index", &n.to_string());
        let js = templates.render("plot.js.tera", &context)?;

        let file_path = plot_path.to_owned() + "plot_" + &n.to_string() + ".js";
        let mut file = fs::File::create(file_path)?;
        file.write_all(js.as_bytes())?;
    }

    let index_path = output_path.to_owned() + "/indexes/";
    fs::create_dir(Path::new(&index_path)).context(WriteErr::CantCreateDir {
        dir_path: index_path.to_owned(),
    })?;

    let data_path = output_path.to_owned() + "/data/";
    fs::create_dir(Path::new(&data_path)).context(WriteErr::CantCreateDir {
        dir_path: data_path.to_owned(),
    })?;

    let mut prefixes = make_prefixes(
        table
            .clone()
            .into_iter()
            .map(|hm| {
                hm.into_iter()
                    .filter(|(k, _)| !is_numeric.get(k.as_str()).unwrap())
                    .collect()
            })
            .collect(),
        titles
            .clone()
            .into_iter()
            .filter(|e| !is_numeric.get(e).unwrap())
            .collect(),
        rows_per_page,
    );

    let bin = make_bins(
        table
            .clone()
            .into_iter()
            .map(|hm| {
                hm.into_iter()
                    .filter(|(k, _)| {
                        *is_numeric.get(k.as_str()).unwrap() && !is_integer.get(k.as_str()).unwrap()
                    })
                    .collect()
            })
            .collect(),
        titles
            .clone()
            .into_iter()
            .filter(|e| *is_numeric.get(e).unwrap() && !is_integer.get(e).unwrap())
            .collect(),
        rows_per_page,
    );

    let int_bin = make_bins_for_integers(
        table
            .clone()
            .into_iter()
            .map(|hm| {
                hm.into_iter()
                    .filter(|(k, _)| *is_integer.get(k.as_str()).unwrap())
                    .collect()
            })
            .collect(),
        titles
            .clone()
            .into_iter()
            .filter(|e| *is_integer.get(e).unwrap())
            .collect(),
        rows_per_page,
    );

    for (k, v) in bin.into_iter().chain(int_bin) {
        prefixes.insert(k, v);
    }

    let prefix_path = output_path.to_owned() + "/prefixes/";
    fs::create_dir(Path::new(&prefix_path)).context(WriteErr::CantCreateDir {
        dir_path: prefix_path.to_owned(),
    })?;

    for (n, title) in titles.iter().enumerate() {
        if let Some(prefix_table) = prefixes.get(title.to_owned()) {
            let mut templates = Tera::default();
            templates.add_raw_template(
                "prefix_table.html.tera",
                include_str!("prefix_table.html.tera"),
            )?;
            let mut context = Context::new();
            context.insert("title", title);
            context.insert("index", &n.to_string());
            context.insert("table", prefix_table);
            context.insert("numeric", is_numeric.get(title).unwrap());
            let html = templates.render("prefix_table.html.tera", &context)?;

            let file_path = output_path.to_owned() + "/prefixes/col_" + &n.to_string() + ".html";
            let mut file = fs::File::create(file_path)?;
            file.write_all(html.as_bytes())?;

            let title_path = prefix_path.to_owned() + "/col_" + &n.to_string() + "/";
            fs::create_dir(Path::new(&title_path)).context(WriteErr::CantCreateDir {
                dir_path: title_path.to_owned(),
            })?;

            for (prefix, values) in prefix_table {
                let mut templates = Tera::default();
                templates.add_raw_template(
                    "lookup_table.html.tera",
                    include_str!("lookup_table.html.tera"),
                )?;
                let mut context = Context::new();
                context.insert("title", title);
                context.insert("values", values);
                context.insert("index", &n.to_string());
                let html = templates.render("lookup_table.html.tera", &context)?;

                let file_path = title_path.to_owned() + prefix + ".html";
                let mut file = fs::File::create(file_path)?;
                file.write_all(html.as_bytes())?;
            }
        }
    }

    let formatter_object = if let Some(f) = formatter {
        let mut file_string = "".to_string();
        let mut custom_file =
            File::open(f).context("Unable to open given file for formatting colums")?;
        custom_file
            .read_to_string(&mut file_string)
            .context("Unable to read string from formatting file")?;

        Some(file_string)
    } else {
        None
    };

    let pinned_columns = if let Some(col) = pin_until {
        titles.iter().position(|&r| r == col).context(
            "Given value for --pin-until did not match any of the columns of your csv file",
        )? + 1
    } else {
        0
    };

    let mut templates = Tera::default();
    templates.add_raw_template("csv_report.js.tera", include_str!("csv_report.js.tera"))?;
    let mut context = Context::new();
    context.insert("titles", &titles);
    context.insert("num", &is_numeric);
    context.insert("formatter", &formatter_object);
    context.insert("pinned_columns", &pinned_columns);
    context.insert("pin", &pin_until.is_some());

    let js = templates.render("csv_report.js.tera", &context)?;

    let file_path = output_path.to_owned() + "/js/csv_report.js";
    let mut file = fs::File::create(file_path)?;
    file.write_all(js.as_bytes())?;

    if table.is_empty() {
        let mut templates = Tera::default();
        templates.add_raw_template("csv_report.html.tera", include_str!("csv_report.html.tera"))?;
        templates.add_raw_template("data.js.tera", include_str!("data.js.tera"))?;
        let mut context = Context::new();
        context.insert("table", &table);
        context.insert("titles", &titles);
        context.insert("current_page", &1);
        context.insert("pages", &1);
        let local: DateTime<Local> = Local::now();
        context.insert("time", &local.format("%a %b %e %T %Y").to_string());
        context.insert("version", &env!("CARGO_PKG_VERSION"));
        context.insert("is_reasonable", &reasonable_plot);

        let data: Vec<Vec<&str>> = Vec::new();

        context.insert(
            "data",
            &json!(compress_to_utf16(&json!(data).to_string())).to_string(),
        );

        let js = templates.render("data.js.tera", &context)?;
        let js_file_path = output_path.to_owned() + "/data/index1.js";
        let mut js_file = fs::File::create(js_file_path)?;
        js_file.write_all(js.as_bytes())?;

        let html = templates.render("csv_report.html.tera", &context)?;
        let file_path = output_path.to_owned() + "/indexes/index1.html";
        let mut file = fs::File::create(file_path)?;
        file.write_all(html.as_bytes())?;
    } else {
        for (i, current_table) in table.chunks(rows_per_page).enumerate() {
            let page = i + 1;

            let mut templates = Tera::default();
            templates
                .add_raw_template("csv_report.html.tera", include_str!("csv_report.html.tera"))?;
            templates.add_raw_template("data.js.tera", include_str!("data.js.tera"))?;
            let mut context = Context::new();
            context.insert("table", &current_table);
            context.insert("titles", &titles);
            context.insert("current_page", &page);
            context.insert("pages", &(pages + 1));
            let local: DateTime<Local> = Local::now();
            context.insert("time", &local.format("%a %b %e %T %Y").to_string());
            context.insert("version", &env!("CARGO_PKG_VERSION"));
            context.insert("is_reasonable", &reasonable_plot);

            let mut data = Vec::new();
            for row in current_table {
                let mut r = Vec::new();
                for title in &titles {
                    r.push(row.get(*title).unwrap())
                }
                data.push(r);
            }

            context.insert(
                "data",
                &json!(compress_to_utf16(&json!(data).to_string())).to_string(),
            );

            let html = templates.render("csv_report.html.tera", &context)?;
            let js = templates.render("data.js.tera", &context)?;

            let file_path = output_path.to_owned() + "/indexes/index" + &page.to_string() + ".html";
            let mut file = fs::File::create(file_path)?;
            file.write_all(html.as_bytes())?;

            let js_file_path = output_path.to_owned() + "/data/index" + &page.to_string() + ".js";
            let mut js_file = fs::File::create(js_file_path)?;
            js_file.write_all(js.as_bytes())?;
        }
    }
    Ok(())
}

fn num_plot(table: &[HashMap<String, String>], column: String) -> Vec<BinnedPlotRecord> {
    let mut values = Vec::new();
    let mut nan = 0;
    for row in table {
        match f32::from_str(row.get(&column).unwrap()) {
            Ok(val) => values.push(val.to_owned()),
            _ => nan += 1,
        }
    }
    let min = values.iter().fold(f32::INFINITY, |a, &b| a.min(b));
    let max = values.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
    let bins = 20;
    let step = (max - min) / bins as f32;
    let mut binned_data = HashMap::new();
    let mut bin_borders = HashMap::new();
    for val in values {
        for i in 0..bins {
            let lower_bound = min + i as f32 * step;
            let upper_bound = lower_bound + step;
            let bin_name = String::from("bin") + &i.to_string();
            bin_borders.insert(bin_name.to_owned(), (lower_bound, upper_bound));
            let entry = binned_data.entry(bin_name.to_owned()).or_insert_with(|| 0);
            if ((i < (bins - 1) && val < upper_bound) || (i < bins && val <= upper_bound))
                && val >= lower_bound
            {
                *entry += 1;
            }
        }
    }
    if nan > 0 {
        bin_borders.insert(
            String::from("bin") + &bins.to_string(),
            (f32::NAN, f32::NAN),
        );
        binned_data.insert(String::from("bin") + &bins.to_string(), nan);
    }
    let mut plot_data = Vec::new();
    for (name, v) in binned_data {
        let (lower_bound, upper_bound) = bin_borders.get(&name).unwrap();
        let plot_record = BinnedPlotRecord {
            bin_start: *lower_bound,
            value: v,
            bin_end: *upper_bound,
        };
        plot_data.push(plot_record);
    }
    plot_data
}

fn nominal_plot(table: &[HashMap<String, String>], column: String) -> Option<Vec<PlotRecord>> {
    let values = table
        .iter()
        .map(|row| row.get(&column).unwrap().to_owned())
        .filter(|s| !s.is_empty())
        .collect_vec();

    let mut count_values = HashMap::new();
    for v in values {
        let entry = count_values.entry(v.to_owned()).or_insert_with(|| 0);
        *entry += 1;
    }

    let mut plot_data = count_values
        .iter()
        .map(|(k, v)| PlotRecord {
            key: k.to_owned(),
            value: *v,
        })
        .collect_vec();

    if plot_data.len() > 10 {
        let unique_values: HashSet<_> = count_values.iter().map(|(_, v)| v).collect();
        if unique_values.len() <= 1 {
            return None;
        };
        plot_data.sort_by(|a, b| b.value.cmp(&a.value));
        plot_data = plot_data.into_iter().take(10).collect();
    }

    Some(plot_data)
}

fn make_prefixes(
    table: Vec<HashMap<String, String>>,
    titles: Vec<&str>,
    rows_per_page: usize,
) -> LookupTable {
    let mut title_map = HashMap::new();
    for (i, partial_table) in table.chunks(rows_per_page).enumerate() {
        let page = i + 1;
        let prefix_len = 3;
        for (index, row) in partial_table.iter().enumerate() {
            for key in &titles {
                let value = &row[key.to_owned()].trim().to_owned();
                if !value.is_empty() {
                    let entry = value.split_whitespace().take(1).collect_vec()[0];
                    if entry.len() >= prefix_len {
                        let prefix = entry.chars().take(prefix_len).collect::<String>();
                        let prefix_map = title_map
                            .entry(key.to_string())
                            .or_insert_with(HashMap::new);
                        let values = prefix_map.entry(prefix).or_insert_with(Vec::new);
                        values.push((value.to_owned(), page, index));
                    }
                }
            }
        }
        // write stuff to output map with page like so: HashMap<column_title, HashMap<prefix, Vec<(value, page, index)>>>
    }
    title_map
}

fn make_bins(
    table: Vec<HashMap<String, String>>,
    titles: Vec<&str>,
    rows_per_page: usize,
) -> LookupTable {
    let mut title_map = HashMap::new();
    for title in titles {
        let mut values = Vec::new();
        for row in &table {
            if let Ok(val) = f32::from_str(row.get(title).unwrap()) {
                values.push(val.to_owned())
            }
        }
        let min = values.iter().fold(f32::INFINITY, |a, &b| a.min(b));
        let max = values.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        let bins = 20;
        let step = (max - min) / bins as f32;
        let mut bin_data = HashMap::new();
        for val in values {
            for i in 0..bins {
                let lower_bound = min + i as f32 * step;
                let upper_bound = lower_bound + step;
                let bin_name = lower_bound.to_string() + "-" + &upper_bound.to_string();
                let entry = bin_data
                    .entry(bin_name.to_owned())
                    .or_insert_with(HashSet::new);
                if ((i < (bins - 1) && val < upper_bound) || (i < bins && val <= upper_bound))
                    && val >= lower_bound
                {
                    entry.insert(val.to_string());
                }
            }
        }

        let mut value_on_page = HashMap::new();
        for (i, partial_table) in table.chunks(rows_per_page).enumerate() {
            let page = i + 1;
            for (index, row) in partial_table.iter().enumerate() {
                if let Ok(val) = f32::from_str(row.get(title).unwrap()) {
                    let entry = value_on_page
                        .entry(val.to_string())
                        .or_insert_with(HashSet::new);
                    entry.insert((page, index));
                }
            }
            // write stuff to output map with page like so: HashMap<column_title, HashMap<bin, Vec<(value, page, index)>>>
        }
        let mut bin_map = HashMap::new();
        for (bin, values) in bin_data {
            for v in values {
                let entry = bin_map.entry(bin.to_string()).or_insert_with(Vec::new);
                for (page, index) in value_on_page.get(&v).unwrap() {
                    entry.push((v.to_string(), *page, *index));
                }
            }
        }
        title_map.insert(title.to_string(), bin_map);
    }

    title_map
}

fn make_bins_for_integers(
    table: Vec<HashMap<String, String>>,
    titles: Vec<&str>,
    rows_per_page: usize,
) -> LookupTable {
    let mut title_map = HashMap::new();
    for title in titles {
        let mut values = Vec::new();
        for row in &table {
            if let Ok(val) = i32::from_str(row.get(title).unwrap()) {
                values.push(val.to_owned())
            }
        }
        let min = *values.iter().min().unwrap();
        let max = *values.iter().max().unwrap();
        let bins = 20;
        let step = if max - min <= 20 {
            1
        } else {
            (max - min) / bins
        };
        let mut bin_data = HashMap::new();
        for val in values {
            for i in 0..bins {
                let lower_bound = min + i * step;
                let upper_bound = if i == bins { max } else { lower_bound + step };
                let bin_name = lower_bound.to_string() + "-" + &upper_bound.to_string();
                let entry = bin_data
                    .entry(bin_name.to_owned())
                    .or_insert_with(HashSet::new);
                if ((i < (bins - 1) && val < upper_bound) || (i < bins && val <= upper_bound))
                    && val >= lower_bound
                {
                    entry.insert(val.to_string());
                }
            }
        }

        let mut value_on_page = HashMap::new();
        for (i, partial_table) in table.chunks(rows_per_page).enumerate() {
            let page = i + 1;
            for (index, row) in partial_table.iter().enumerate() {
                if let Ok(val) = i32::from_str(row.get(title).unwrap()) {
                    let entry = value_on_page
                        .entry(val.to_string())
                        .or_insert_with(HashSet::new);
                    entry.insert((page, index));
                }
            }
            // write stuff to output map with page like so: HashMap<column_title, HashMap<bin, Vec<(value, page, index)>>>
        }
        let mut bin_map = HashMap::new();
        for (bin, values) in bin_data {
            for v in values {
                let entry = bin_map.entry(bin.to_string()).or_insert_with(Vec::new);
                for (page, index) in value_on_page.get(&v).unwrap() {
                    entry.push((v.to_string(), *page, *index));
                }
            }
        }
        title_map.insert(title.to_string(), bin_map);
    }

    title_map
}

#[derive(new, Serialize, Debug, Clone)]
struct PlotRecord {
    key: String,
    value: u32,
}

#[derive(new, Serialize, Debug, Clone)]
struct BinnedPlotRecord {
    bin_start: f32,
    bin_end: f32,
    value: u32,
}

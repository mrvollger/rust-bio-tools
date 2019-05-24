//! Tool to remove PCR duplicates from UMI-tagged reads.
//!
//! This tool takes two FASTQ files (forward and reverse)
//! and returns two FASTQ files in which all PCR duplicates
//! have been merged into a consensus read.
//! Duplicates are identified by a Unique Molecular Identifier (UMI).
//!
//! ## Requirements:
//!
//!  - starcode
//!
//!
//! ## Usage:
//!
//! ```bash
//! $ rbt call-consensus-reads \
//!   <Path to FASTQ file with forward reads> \
//!   <Path to FASTQ file with reverse reads> \
//!   <Path for output forward FASTQ file> \
//!   <Path for output reverse FASTQ file> \
//!   -l <Length of UMI sequence> \
//!   -D <Maximum distance between sequences in a cluster> \  # See step 1 below
//!   -d <Maximum distance between UMIs in a cluster> \  # See step 2 below
//!   --umi-on-reverse  # if the UMIs are part of the reverse reads
//! ```
//!
//! ## Assumptions:
//!
//!  - Reads are of equal length
//!  - UMI is the prefix of the reads
//!
//! ## Workflow:
//!
//! The main steps are:
//!
//! 1. Preparation
//!    1. Remove UMI sequence from read (and save it for later use).
//!    2. Concatenate forward and reverse sequence.
//!        ```text
//!        Forward Read: [================]
//!        Reverse Read: [(UMI)-----------]
//!        Sequence for clustering in step 3: [================-----------]
//!        ```
//!
//! 2. Cluster all reads by their UMIs using starcode.
//!    Each cluster generated in this step contains reads with similar UMIs.
//!    However, all PCR duplicates of a read are within one cluster, since they
//!    share a UMI sequence.
//!    The size of these clusters highly depends on the length of the used UMI.
//!
//! 2. For each cluster from step two:
//!    1. Cluster reads by their concatenated sequences (without UMI) using starcode.
//!    2. Each new cluster contains reads that have a similar UMI (from step 2)
//!       as well as similar sequences. Consequently, these sets of reads are
//!       likely to be PCR duplicates of each other.
//!
//! 3. For each cluster from step three: Compute a consensus sequence.
//!
//!    At each position in the read, all bases and quality values are used
//!    to compute the base with Maximum a-posteriori probability (MAP).
//!
//!      1. For one position, compute the likelihood for the four alleles
//!         A, C, G, and T, incorporating the number of bases as well as
//!         their quality values.
//!      2. Choose the allele with the largest likelihood for the consensus read.
//!      3. Compute the quality value of the consensus read from the maximum posterior
//!         probability used to select the allele.
//!
//! 4. Write consensus reads to output file.
//!
//!
//!
// Since this is a binary crate, documentation needs to be compiled with this 'ancient incantation':
// https://github.com/rust-lang/cargo/issues/1865#issuecomment-394179125
mod calc_consensus;
mod pipeline;

use crate::errors::{self, Result, Error};

use bio::io::fastq;
use flate2::bufread::MultiGzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use pipeline::{CallConsensusReads, CallNonOverlappingConsensusRead, CallOverlappingConsensusRead};
use snafu::{ResultExt};
use std::fs;
use std::io::BufReader;
use std::str;

/// Format parameters into a string to provide error context for the
/// call to `call_consensus_reads()`
fn format_pipeline_params(
    umi_len: usize,
    seq_dist: usize,
    umi_dist: usize,
    reverse_umi: bool,
    verbose_read_names: bool,
    insert_size: Option<usize>,
    std_dev: Option<usize>,
) -> String {
    let umi_pos = match reverse_umi {
        true => format!(
            "UMIs are the first {} characters of the reverse read.",
            umi_len
        ),
        false => format!(
            "UMIs are the first {} characters of the forward read.",
            umi_len
        ),
    };
    let verbose_reads = match verbose_read_names {
        true => "Read names are written in verbose format.",
        false => "Read names are written in short format.",
    };
    let mode = match (insert_size, std_dev) {
        (Some(is), Some(sd)) => format!(
            "Run in overlap mode with insert size {} and std deviation {}.",
            is, sd
        ),
        (None, None) => String::from("Run in normal mode without overlaps."),
        _ => String::from("Invalid mode."), // This cannot occur due to the clap configuration.
    };
    format!(
        "Pipeline did not finish correctly. It was run with \
         sequence distance {} and UMI distance {}.\n{}\n{}\n{}",
        seq_dist, umi_dist, umi_pos, verbose_reads, mode,
    )
}

/// Build readers for the given input and output FASTQ files and pass them to
/// `call_consensus_reads`.
///
/// The type of the readers (writers) depends on the file ending.
/// If the input file names end with '.gz' a gzipped reader (writer) is used.
pub fn call_consensus_reads_from_paths(
    fq1: &str,
    fq2: &str,
    fq1_out: &str,
    fq2_out: &str,
    fq3_out: Option<&str>,
    umi_len: usize,
    seq_dist: usize,
    umi_dist: usize,
    reverse_umi: bool,
    verbose_read_names: bool,
    insert_size: Option<usize>,
    std_dev: Option<usize>,
) -> errors::Result<()> {
    match fq3_out {
        None => {
            eprintln!("Reading input files:\n    {}\n    {}", fq1, fq2);
            eprintln!("Writing output to:\n    {}\n    {}", fq1_out, fq2_out);
            match (
                fq1.ends_with(".gz"),
                fq2.ends_with(".gz"),
                fq1_out.ends_with(".gz"),
                fq2_out.ends_with(".gz"),
            ) {
                (false, false, false, false) => CallNonOverlappingConsensusRead::new(
                    &mut fastq::Reader::from_file(fq1).context(errors::ReaderError {
                        filename: String::from(fq1),
                    })?,
                    &mut fastq::Reader::from_file(fq2).context(errors::ReaderError {
                        filename: String::from(fq2),
                    })?,
                    &mut fastq::Writer::to_file(fq1_out).context(errors::WriterError {
                        filename: String::from(fq1_out),
                    })?,
                    &mut fastq::Writer::to_file(fq2_out).context(errors::WriterError {
                        filename: String::from(fq2_out),
                    })?,
                    umi_len,
                    seq_dist,
                    umi_dist,
                    reverse_umi,
                    verbose_read_names,
                )
                .call_consensus_reads()
                .context(errors::PipelineError {
                    params: format_pipeline_params(
                        umi_len,
                        seq_dist,
                        umi_dist,
                        reverse_umi,
                        verbose_read_names,
                        insert_size,
                        std_dev,
                    ),
                }),
                (true, true, false, false) => CallNonOverlappingConsensusRead::new(
                    &mut fastq::Reader::new(
                        fs::File::open(fq1)
                            .map(BufReader::new)
                            .map(MultiGzDecoder::new)
                            .context(errors::ReaderError {
                                filename: String::from(fq1),
                            })?,
                    ),
                    &mut fastq::Reader::new(
                        fs::File::open(fq2)
                            .map(BufReader::new)
                            .map(MultiGzDecoder::new)
                            .context(errors::ReaderError {
                                filename: String::from(fq2),
                            })?,
                    ),
                    &mut fastq::Writer::to_file(fq1_out).context(errors::ReaderError {
                        filename: String::from(fq1_out),
                    })?,
                    &mut fastq::Writer::to_file(fq2_out).context(errors::ReaderError {
                        filename: String::from(fq2_out),
                    })?,
                    umi_len,
                    seq_dist,
                    umi_dist,
                    reverse_umi,
                    verbose_read_names,
                )
                .call_consensus_reads()
                .context(errors::PipelineError {
                    params: format_pipeline_params(
                        umi_len,
                        seq_dist,
                        umi_dist,
                        reverse_umi,
                        verbose_read_names,
                        insert_size,
                        std_dev,
                    ),
                }),
                (false, false, true, true) => CallNonOverlappingConsensusRead::new(
                    &mut fastq::Reader::from_file(fq1).context(errors::ReaderError {
                        filename: String::from(fq1),
                    })?,
                    &mut fastq::Reader::from_file(fq2).context(errors::ReaderError {
                        filename: String::from(fq2),
                    })?,
                    &mut fastq::Writer::new(GzEncoder::new(
                        fs::File::create(fq1_out).context(errors::ReaderError {
                            filename: String::from(fq1_out),
                        })?,
                        Compression::default(),
                    )),
                    &mut fastq::Writer::new(GzEncoder::new(
                        fs::File::create(fq2_out).context(errors::ReaderError {
                            filename: String::from(fq2_out),
                        })?,
                        Compression::default(),
                    )),
                    umi_len,
                    seq_dist,
                    umi_dist,
                    reverse_umi,
                    verbose_read_names,
                )
                .call_consensus_reads()
                .context(errors::PipelineError {
                    params: format_pipeline_params(
                        umi_len,
                        seq_dist,
                        umi_dist,
                        reverse_umi,
                        verbose_read_names,
                        insert_size,
                        std_dev,
                    ),
                }),
                (true, true, true, true) => CallNonOverlappingConsensusRead::new(
                    &mut fastq::Reader::new(
                        fs::File::open(fq1)
                            .map(BufReader::new)
                            .map(MultiGzDecoder::new)
                            .context(errors::ReaderError {
                                filename: String::from(fq1),
                            })?,
                    ),
                    &mut fastq::Reader::new(
                        fs::File::open(fq2)
                            .map(BufReader::new)
                            .map(MultiGzDecoder::new)
                            .context(errors::ReaderError {
                                filename: String::from(fq2),
                            })?,
                    ),
                    &mut fastq::Writer::new(GzEncoder::new(
                        fs::File::create(fq1_out).context(errors::ReaderError {
                            filename: String::from(fq1_out),
                        })?,
                        Compression::default(),
                    )),
                    &mut fastq::Writer::new(GzEncoder::new(
                        fs::File::create(fq2_out).context(errors::ReaderError {
                            filename: String::from(fq2_out),
                        })?,
                        Compression::default(),
                    )),
                    umi_len,
                    seq_dist,
                    umi_dist,
                    reverse_umi,
                    verbose_read_names,
                )
                .call_consensus_reads()
                .context(errors::PipelineError {
                    params: format_pipeline_params(
                        umi_len,
                        seq_dist,
                        umi_dist,
                        reverse_umi,
                        verbose_read_names,
                        insert_size,
                        std_dev,
                    ),
                }),
                _ => panic!(
                    "Invalid combination of files. Each pair of files \
                     (input and output) need to be both gzipped or \
                     both not zipped."
                ),
            }
        }
        Some(fq3_out) => {
            eprintln!("Reading input files:\n    {}\n    {}", fq1, fq2);
            eprintln!(
                "Writing output to:\n    {}\n    {}\n    {}",
                fq1_out, fq2_out, fq3_out
            );
            match (
                fq1.ends_with(".gz"),
                fq2.ends_with(".gz"),
                fq1_out.ends_with(".gz"),
                fq2_out.ends_with(".gz"),
                fq3_out.ends_with(".gz"),
            ) {
                (false, false, false, false, false) => CallOverlappingConsensusRead::new(
                    &mut fastq::Reader::from_file(fq1).context(errors::ReaderError {
                        filename: String::from(fq1),
                    })?,
                    &mut fastq::Reader::from_file(fq2).context(errors::ReaderError {
                        filename: String::from(fq2),
                    })?,
                    &mut fastq::Writer::to_file(fq1_out).context(errors::ReaderError {
                        filename: String::from(fq1_out),
                    })?,
                    &mut fastq::Writer::to_file(fq2_out).context(errors::ReaderError {
                        filename: String::from(fq2_out),
                    })?,
                    &mut fastq::Writer::to_file(fq3_out).context(errors::ReaderError {
                        filename: String::from(fq3_out),
                    })?,
                    umi_len,
                    seq_dist,
                    umi_dist,
                    insert_size.unwrap(),
                    std_dev.unwrap(),
                    reverse_umi,
                    verbose_read_names,
                )
                .call_consensus_reads()
                .context(errors::PipelineError {
                    params: format_pipeline_params(
                        umi_len,
                        seq_dist,
                        umi_dist,
                        reverse_umi,
                        verbose_read_names,
                        insert_size,
                        std_dev,
                    ),
                }),
                (true, true, false, false, false) => CallOverlappingConsensusRead::new(
                    &mut fastq::Reader::new(
                        fs::File::open(fq1)
                            .map(BufReader::new)
                            .map(MultiGzDecoder::new)
                            .context(errors::ReaderError {
                                filename: String::from(fq1),
                            })?,
                    ),
                    &mut fastq::Reader::new(
                        fs::File::open(fq2)
                            .map(BufReader::new)
                            .map(MultiGzDecoder::new)
                            .context(errors::ReaderError {
                                filename: String::from(fq2),
                            })?,
                    ),
                    &mut fastq::Writer::to_file(fq1_out).context(errors::ReaderError {
                        filename: String::from(fq1_out),
                    })?,
                    &mut fastq::Writer::to_file(fq2_out).context(errors::ReaderError {
                        filename: String::from(fq2_out),
                    })?,
                    &mut fastq::Writer::to_file(fq3_out).context(errors::ReaderError {
                        filename: String::from(fq3_out),
                    })?,
                    umi_len,
                    seq_dist,
                    umi_dist,
                    insert_size.unwrap(),
                    std_dev.unwrap(),
                    reverse_umi,
                    verbose_read_names,
                )
                .call_consensus_reads()
                .context(errors::PipelineError {
                    params: format_pipeline_params(
                        umi_len,
                        seq_dist,
                        umi_dist,
                        reverse_umi,
                        verbose_read_names,
                        insert_size,
                        std_dev,
                    ),
                }),
                (false, false, true, true, true) => CallOverlappingConsensusRead::new(
                    &mut fastq::Reader::from_file(fq1).context(errors::ReaderError {
                        filename: String::from(fq1),
                    })?,
                    &mut fastq::Reader::from_file(fq2).context(errors::ReaderError {
                        filename: String::from(fq2),
                    })?,
                    &mut fastq::Writer::new(GzEncoder::new(
                        fs::File::create(fq1_out).context(errors::ReaderError {
                            filename: String::from(fq1_out),
                        })?,
                        Compression::default(),
                    )),
                    &mut fastq::Writer::new(GzEncoder::new(
                        fs::File::create(fq2_out).context(errors::ReaderError {
                            filename: String::from(fq2_out),
                        })?,
                        Compression::default(),
                    )),
                    &mut fastq::Writer::new(GzEncoder::new(
                        fs::File::create(fq3_out).context(errors::ReaderError {
                            filename: String::from(fq3_out),
                        })?,
                        Compression::default(),
                    )),
                    umi_len,
                    seq_dist,
                    umi_dist,
                    insert_size.unwrap(),
                    std_dev.unwrap(),
                    reverse_umi,
                    verbose_read_names,
                )
                .call_consensus_reads()
                .context(errors::PipelineError {
                    params: format_pipeline_params(
                        umi_len,
                        seq_dist,
                        umi_dist,
                        reverse_umi,
                        verbose_read_names,
                        insert_size,
                        std_dev,
                    ),
                }),
                (true, true, true, true, true) => CallOverlappingConsensusRead::new(
                    &mut fastq::Reader::new(
                        fs::File::open(fq1)
                            .map(BufReader::new)
                            .map(MultiGzDecoder::new)
                            .context(errors::ReaderError {
                                filename: String::from(fq1),
                            })?,
                    ),
                    &mut fastq::Reader::new(
                        fs::File::open(fq2)
                            .map(BufReader::new)
                            .map(MultiGzDecoder::new)
                            .context(errors::ReaderError {
                                filename: String::from(fq2),
                            })?,
                    ),
                    &mut fastq::Writer::new(GzEncoder::new(
                        fs::File::create(fq1_out).context(errors::ReaderError {
                            filename: String::from(fq1_out),
                        })?,
                        Compression::default(),
                    )),
                    &mut fastq::Writer::new(GzEncoder::new(
                        fs::File::create(fq2_out).context(errors::ReaderError {
                            filename: String::from(fq2_out),
                        })?,
                        Compression::default(),
                    )),
                    &mut fastq::Writer::new(GzEncoder::new(
                        fs::File::create(fq3_out).context(errors::ReaderError {
                            filename: String::from(fq3_out),
                        })?,
                        Compression::default(),
                    )),
                    umi_len,
                    seq_dist,
                    umi_dist,
                    insert_size.unwrap(),
                    std_dev.unwrap(),
                    reverse_umi,
                    verbose_read_names,
                )
                .call_consensus_reads()
                .context(errors::PipelineError {
                    params: format_pipeline_params(
                        umi_len,
                        seq_dist,
                        umi_dist,
                        reverse_umi,
                        verbose_read_names,
                        insert_size,
                        std_dev,
                    ),
                    
                }),
                _ => panic!(
                    "Invalid combination of files. Each pair of files \
                     (input and output) need to be both gzipped or \
                     both not zipped."
                ),
            }
        }
    }
}

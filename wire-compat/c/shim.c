// builds reliable_config_t inside C, so the Rust test crate never mirrors the
// struct layout and cannot get it wrong

#include "reliable.h"

typedef void (*wire_compat_transmit_fn)( void *, uint64_t, uint16_t, uint8_t *, int );
typedef int (*wire_compat_process_fn)( void *, uint64_t, uint16_t, uint8_t *, int );

struct reliable_endpoint_t * wire_compat_endpoint_create( int fragment_above,
                                                          uint64_t id,
                                                          void * context,
                                                          wire_compat_transmit_fn transmit,
                                                          wire_compat_process_fn process )
{
    struct reliable_config_t config;
    reliable_default_config( &config );
    config.fragment_above = fragment_above;
    config.id = id;
    config.context = context;
    config.transmit_packet_function = transmit;
    config.process_packet_function = process;
    return reliable_endpoint_create( &config, 100.0 );
}

// ---------------------------------------------------------------
// differential oracle
//
// runs a sequence of raw datagrams through one C endpoint with in-memory callbacks and
// formats, per datagram, one line the port's differential test can compare byte for byte.
// the eleven counters use the reliable.h order and names.

#include <stdio.h>
#include <stdlib.h>

struct wire_compat_diff_t
{
    struct reliable_endpoint_t * endpoint;
    int num_processed;
    int last_sequence;
    int last_bytes;
    uint64_t previous_counters[RELIABLE_ENDPOINT_NUM_COUNTERS];
};

static const char * wire_compat_diff_counter_names[RELIABLE_ENDPOINT_NUM_COUNTERS] =
{
    "num_packets_sent",
    "num_packets_received",
    "num_packets_acked",
    "num_packets_stale",
    "num_packets_invalid",
    "num_packets_too_large_to_send",
    "num_packets_too_large_to_receive",
    "num_fragments_sent",
    "num_fragments_received",
    "num_fragments_invalid",
    "num_packets_duplicate"
};

static void wire_compat_diff_transmit( void * context, uint64_t id, uint16_t sequence, uint8_t * packet_data, int packet_bytes )
{
    (void) context; (void) id; (void) sequence; (void) packet_data; (void) packet_bytes;
}

static int wire_compat_diff_process( void * context, uint64_t id, uint16_t sequence, uint8_t * packet_data, int packet_bytes )
{
    (void) id; (void) packet_data; (void) packet_bytes;
    struct wire_compat_diff_t * diff = (struct wire_compat_diff_t*) context;
    diff->num_processed++;
    diff->last_sequence = sequence;
    diff->last_bytes = packet_bytes;
    return 1;
}

void * wire_compat_diff_create( int fragment_above )
{
    struct wire_compat_diff_t * diff = (struct wire_compat_diff_t*) calloc( 1, sizeof( struct wire_compat_diff_t ) );
    if ( !diff )
        return NULL;
    diff->endpoint = wire_compat_endpoint_create( fragment_above, 0, diff, wire_compat_diff_transmit, wire_compat_diff_process );
    if ( !diff->endpoint )
    {
        free( diff );
        return NULL;
    }
    return diff;
}

void wire_compat_diff_destroy( void * handle )
{
    struct wire_compat_diff_t * diff = (struct wire_compat_diff_t*) handle;
    if ( !diff )
        return;
    reliable_endpoint_destroy( diff->endpoint );
    free( diff );
}

// feeds one datagram and writes one transcript line. returns the number of bytes written,
// excluding the terminator, or -1 if the output buffer is too small.

int wire_compat_diff_feed( void * handle, const uint8_t * data, int len, int n, char * out, int cap )
{
    struct wire_compat_diff_t * diff = (struct wire_compat_diff_t*) handle;

    const int processed_before = diff->num_processed;

    reliable_endpoint_receive_packet( diff->endpoint, (uint8_t*) data, len );

    const int delivered = diff->num_processed != processed_before;

    RELIABLE_CONST uint64_t * counters = reliable_endpoint_counters( diff->endpoint );

    const char * changed = "none";
    for ( int i = 0; i < RELIABLE_ENDPOINT_NUM_COUNTERS; ++i )
    {
        if ( counters[i] != diff->previous_counters[i] )
        {
            changed = wire_compat_diff_counter_names[i];
            break;
        }
    }
    for ( int i = 0; i < RELIABLE_ENDPOINT_NUM_COUNTERS; ++i )
        diff->previous_counters[i] = counters[i];

    int written = snprintf( out, (size_t) cap,
        "%d %s %s %s ",
        n,
        delivered ? "accepted" : "refused",
        changed,
        delivered ? "" : "-" );
    if ( written < 0 || written >= cap )
        return -1;

    if ( delivered )
    {
        int more = snprintf( out + written, (size_t) ( cap - written ), "%d:%d ", diff->last_sequence, diff->last_bytes );
        if ( more < 0 || more >= cap - written )
            return -1;
        written += more;
    }

    for ( int i = 0; i < RELIABLE_ENDPOINT_NUM_COUNTERS; ++i )
    {
        int more = snprintf( out + written, (size_t) ( cap - written ), "%llu%s", (unsigned long long) counters[i], i + 1 < RELIABLE_ENDPOINT_NUM_COUNTERS ? " " : "" );
        if ( more < 0 || more >= cap - written )
            return -1;
        written += more;
    }

    return written;
}

// ---------------------------------------------------------------
// fragment oracle
//
// one crafted-input line per call, on a fresh endpoint:
//   <case> <fragment seq> <refused|accepted> <genuine seq 0 delivered 0|1> <eleven counters>
// feeds an optional advance packet, then one or two fragment datagrams, then a genuine
// sequence 0 packet. a fragment is "refused" when it bumped NUM_FRAGMENTS_INVALID, else
// "accepted". delivered is whether any process callback ran across the fragments and the
// genuine packet, so a completed reassembly counts. fragment_sequence is reported as given.

int wire_compat_frag_oracle( const char * case_name,
                             int fragment_above,
                             int fragment_sequence,
                             const uint8_t * advance, int advance_len,
                             const uint8_t * fragment1, int fragment1_len,
                             const uint8_t * fragment2, int fragment2_len,
                             const uint8_t * genuine, int genuine_len,
                             char * out, int cap )
{
    struct wire_compat_diff_t * diff = (struct wire_compat_diff_t*) calloc( 1, sizeof( struct wire_compat_diff_t ) );
    if ( !diff )
        return -1;
    diff->endpoint = wire_compat_endpoint_create( fragment_above, 0, diff, wire_compat_diff_transmit, wire_compat_diff_process );
    if ( !diff->endpoint )
    {
        free( diff );
        return -1;
    }

    if ( advance_len > 0 )
        reliable_endpoint_receive_packet( diff->endpoint, (uint8_t*) advance, advance_len );

    const int processed_before = diff->num_processed;
    const uint64_t invalid_before = reliable_endpoint_counters( diff->endpoint )[RELIABLE_ENDPOINT_COUNTER_NUM_FRAGMENTS_INVALID];

    reliable_endpoint_receive_packet( diff->endpoint, (uint8_t*) fragment1, fragment1_len );
    if ( fragment2_len > 0 )
        reliable_endpoint_receive_packet( diff->endpoint, (uint8_t*) fragment2, fragment2_len );

    const uint64_t invalid_after = reliable_endpoint_counters( diff->endpoint )[RELIABLE_ENDPOINT_COUNTER_NUM_FRAGMENTS_INVALID];
    const int refused = invalid_after != invalid_before;

    reliable_endpoint_receive_packet( diff->endpoint, (uint8_t*) genuine, genuine_len );

    const int delivered = diff->num_processed != processed_before;

    RELIABLE_CONST uint64_t * counters = reliable_endpoint_counters( diff->endpoint );

    int written = snprintf( out, (size_t) cap, "%s %d %s %d ",
        case_name, fragment_sequence, refused ? "refused" : "accepted", delivered ? 1 : 0 );
    if ( written < 0 || written >= cap )
    {
        reliable_endpoint_destroy( diff->endpoint );
        free( diff );
        return -1;
    }

    for ( int i = 0; i < RELIABLE_ENDPOINT_NUM_COUNTERS; ++i )
    {
        int more = snprintf( out + written, (size_t) ( cap - written ), "%llu%s", (unsigned long long) counters[i], i + 1 < RELIABLE_ENDPOINT_NUM_COUNTERS ? " " : "" );
        if ( more < 0 || more >= cap - written )
        {
            reliable_endpoint_destroy( diff->endpoint );
            free( diff );
            return -1;
        }
        written += more;
    }

    reliable_endpoint_destroy( diff->endpoint );
    free( diff );

    return written;
}

